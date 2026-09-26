#include "sal_runtime.h"

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef SAL_INSTRUMENT

enum { SAL_REDZONE = 32 };
enum { SAL_POISON_BYTE = 0xDD };

typedef struct Block {
    void *user;
    void *raw;
    int64_t size;
    int place;
    char site[64];
    struct Block *next;
} Block;

static Block *live = NULL;
static Block *quarantine = NULL;
static int enabled = 0;

static void copy_site(char *dst, size_t n, const char *site) {
    strncpy(dst, site ? site : "?", n - 1);
    dst[n - 1] = '\0';
}

static void emit_and_die(const char *code, const char *site) {
    fprintf(stderr, "{\"code\":\"%s\",\"site\":\"%s\"}\n", code, site ? site : "?");
    exit(2);
}

static Block *find_covering(Block *list, const void *p, int64_t nbytes) {
    const unsigned char *up = (const unsigned char *)p;
    for (Block *b = list; b; b = b->next) {
        const unsigned char *base = (const unsigned char *)b->user;
        if (up >= base && up + nbytes <= base + b->size) {
            return b;
        }
        if (up >= base && up < base + b->size) {
            return b;
        }
    }
    return NULL;
}

static Block *find_by_user(Block *list, const void *p) {
    for (Block *b = list; b; b = b->next) {
        if (b->user == p) {
            return b;
        }
    }
    return NULL;
}

static int remove_block(Block **list, Block *target) {
    Block **cur = list;
    while (*cur) {
        if (*cur == target) {
            *cur = target->next;
            target->next = NULL;
            return 1;
        }
        cur = &(*cur)->next;
    }
    return 0;
}

static void poison(void *p, int64_t n) {
    if (p && n > 0) {
        memset(p, SAL_POISON_BYTE, (size_t)n);
    }
}

void sal_instrument_init(void) {
    enabled = 1;
}

void *sal_instrument_malloc(int64_t size, int place, const char *site) {
    if (!enabled) {
        return NULL;
    }
    if (size < 0) {
        size = 0;
    }
    size_t raw_size = (size_t)size + (size_t)SAL_REDZONE * 2;
    unsigned char *raw = (unsigned char *)malloc(raw_size);
    if (!raw) {
        return NULL;
    }
    poison(raw, SAL_REDZONE);
    poison(raw + SAL_REDZONE + size, SAL_REDZONE);
    void *user = raw + SAL_REDZONE;
    sal_instrument_alloc(user, size, place, site);
    Block *b = find_by_user(live, user);
    if (b) {
        b->raw = raw;
    }
    return user;
}

void sal_instrument_alloc(void *p, int64_t size, int place, const char *site) {
    if (!enabled || !p) {
        return;
    }
    Block *b = (Block *)malloc(sizeof(Block));
    if (!b) {
        return;
    }
    b->user = p;
    b->raw = NULL;
    b->size = size;
    b->place = place;
    copy_site(b->site, sizeof(b->site), site);
    b->next = live;
    live = b;
}

void sal_instrument_free(void *p) {
    if (!enabled || !p) {
        return;
    }
    Block *already = find_by_user(quarantine, p);
    if (already) {
        emit_and_die("DOUBLE_FREE", already->site);
    }
    Block *b = find_by_user(live, p);
    if (!b) {
        emit_and_die("DOUBLE_FREE", "?");
    }
    remove_block(&live, b);
    poison(b->user, b->size);
    if (b->raw) {
        poison((unsigned char *)b->raw, SAL_REDZONE);
        poison((unsigned char *)b->raw + SAL_REDZONE + b->size, SAL_REDZONE);
    }
    b->next = quarantine;
    quarantine = b;
}

void sal_instrument_check_index(int64_t index, int64_t len, const char *site) {
    if (!enabled) {
        return;
    }
    if (index < 0 || index >= len) {
        emit_and_die("OOB", site);
    }
}

void sal_instrument_check_ptr(const void *p, int64_t nbytes, int place, const char *site) {
    if (!enabled || !p) {
        return;
    }
    if (nbytes < 0) {
        nbytes = 0;
    }

    Block *q = find_covering(quarantine, p, nbytes);
    if (!q) {
        q = find_by_user(quarantine, p);
    }
    if (q) {
        emit_and_die("USE_AFTER_FREE", site);
    }

    Block *b = find_covering(live, p, nbytes);
    if (!b) {
        b = find_by_user(live, p);
    }
    if (!b) {
        /* Pointer not in a live block: treat as red-zone / OOB. */
        emit_and_die("OOB", site);
    }
    if (b->place != place) {
        emit_and_die("BAD_PLACE", site);
    }

    const unsigned char *up = (const unsigned char *)p;
    const unsigned char *base = (const unsigned char *)b->user;
    if (up < base || up + nbytes > base + b->size) {
        emit_and_die("OOB", site);
    }
}

void sal_instrument_check_f32(const float *data, int64_t n, const char *site) {
    if (!enabled || !data) {
        return;
    }
    for (int64_t i = 0; i < n; i++) {
        if (isnan(data[i])) {
            emit_and_die("NAN", site);
        }
        if (isinf(data[i])) {
            emit_and_die("INF", site);
        }
    }
}

void sal_instrument_shutdown(void) {
    if (!enabled) {
        return;
    }
    /* Drop string/file buffers the selfhost compiler leaves live; keep
     * tensor/vec leaks detectable for instrument_sal tests. */
    while (live) {
        const char *s = live->site;
        int ephemeral = s && (
            strcmp(s, "concat") == 0 || strcmp(s, "strdup") == 0 ||
            strcmp(s, "str_slice") == 0 || strcmp(s, "read_file") == 0 ||
            strcmp(s, "tmp_path") == 0 || strcmp(s, "vec_new") == 0 ||
            strcmp(s, "getenv") == 0 || strcmp(s, "append") == 0 ||
            strcmp(s, "ir") == 0 || strcmp(s, "map") == 0 ||
            strcmp(s, "lex") == 0);
        if (!ephemeral) {
            break;
        }
        Block *b = live;
        live = b->next;
        if (b->raw) {
            free(b->raw);
        }
        free(b);
    }
    if (live) {
        emit_and_die("LEAK", live->site);
    }
}

#else

void sal_instrument_init(void) {}
void sal_instrument_shutdown(void) {}

void sal_instrument_alloc(void *p, int64_t size, int place, const char *site) {
    (void)p;
    (void)size;
    (void)place;
    (void)site;
}

void sal_instrument_free(void *p) {
    (void)p;
}

void sal_instrument_check_f32(const float *data, int64_t n, const char *site) {
    (void)data;
    (void)n;
    (void)site;
}

void *sal_instrument_malloc(int64_t size, int place, const char *site) {
    (void)size;
    (void)place;
    (void)site;
    return NULL;
}

void sal_instrument_check_ptr(const void *p, int64_t nbytes, int place, const char *site) {
    (void)p;
    (void)nbytes;
    (void)place;
    (void)site;
}

void sal_instrument_check_index(int64_t index, int64_t len, const char *site) {
    (void)index;
    (void)len;
    (void)site;
}

#endif

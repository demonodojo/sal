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

/* One lock for the live and quarantine lists: sal threads share them. */
#ifdef SAL_SOSO
#define INST_LOCK() ((void)0)
#define INST_UNLOCK() ((void)0)
#else
#include <pthread.h>
static pthread_mutex_t inst_lock = PTHREAD_MUTEX_INITIALIZER;
#define INST_LOCK() pthread_mutex_lock(&inst_lock)
#define INST_UNLOCK() pthread_mutex_unlock(&inst_lock)
#endif

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

/* A block the caller allocated itself (`raw` is NULL) goes back to malloc when released, so
 * its memory can be handed out again: any such block in quarantine that overlaps the new
 * `[lo, lo + n)` is stale, and a check of the new block must not report it as freed. */
static void purge_reused_locked(const void *lo, int64_t n) {
    const unsigned char *a = (const unsigned char *)lo;
    const unsigned char *z = a + (n > 0 ? n : 1);
    Block **cur = &quarantine;
    while (*cur) {
        Block *b = *cur;
        const unsigned char *base = (const unsigned char *)b->user;
        const unsigned char *end = base + (b->size > 0 ? b->size : 1);
        if (!b->raw && base < z && a < end) {
            *cur = b->next;
            free(b);
        } else {
            cur = &b->next;
        }
    }
}

static Block *track_locked(void *p, int64_t size, int place, const char *site) {
    Block *b = (Block *)malloc(sizeof(Block));
    if (!b) {
        return NULL;
    }
    b->user = p;
    b->raw = NULL;
    b->size = size;
    b->place = place;
    copy_site(b->site, sizeof(b->site), site);
    b->next = live;
    live = b;
    return b;
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
    INST_LOCK();
    purge_reused_locked(raw, (int64_t)raw_size);
    Block *b = track_locked(user, size, place, site);
    if (b) {
        b->raw = raw;
    }
    INST_UNLOCK();
    return user;
}

void sal_instrument_alloc(void *p, int64_t size, int place, const char *site) {
    if (!enabled || !p) {
        return;
    }
    INST_LOCK();
    /* Memory the runtime maps itself (an arena page), or one malloc hands out again, can come
     * back at an address freed before: it is a new block, not the one in quarantine. */
    purge_reused_locked(p, size);
    track_locked(p, size, place, site);
    INST_UNLOCK();
}

void sal_instrument_free(void *p) {
    if (!enabled || !p) {
        return;
    }
    INST_LOCK();
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
    INST_UNLOCK();
}

/* Releases `p` when `sal_instrument_malloc` gave it out (1); a block the caller allocated itself
 * is only untracked (0), and the caller frees it. A block of `sal_instrument_malloc` already in
 * quarantine (its memory is not reused while there) is a DOUBLE_FREE. */
int sal_instrument_free_owned(void *p) {
    if (!enabled || !p) {
        return 0;
    }
    INST_LOCK();
    Block *gone = find_by_user(quarantine, p);
    if (gone && gone->raw) {
        emit_and_die("DOUBLE_FREE", gone->site);
    }
    Block *b = find_by_user(live, p);
    int own = b && b->raw;
    INST_UNLOCK();
    if (b) {
        sal_instrument_free(p);
    }
    return own;
}

/* The size and place of a live block that starts at `p` (1), or 0 when it tracks none. */
int sal_instrument_block_info(const void *p, int64_t *size, int *place) {
    if (!enabled || !p) {
        return 0;
    }
    INST_LOCK();
    Block *b = find_by_user(live, p);
    if (b) {
        *size = b->size;
        *place = b->place;
    }
    INST_UNLOCK();
    return b != NULL;
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

    INST_LOCK();
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
    INST_UNLOCK();
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
    /* The selfhost compiler still keeps strings and handles alive on purpose, so
     * by default those sites are not reported (tensor and container leaks are).
     * Struct boxes of the selfhost AST are in that group too.
     * SAL_INSTRUMENT_STRICT=1 reports every live block: a program whose locals
     * the compiler frees must come out clean. */
    int strict = getenv("SAL_INSTRUMENT_STRICT") != NULL;
    if (getenv("SAL_INSTRUMENT_HIST") != NULL) {
        /* Live blocks and bytes per allocation site, biggest first. */
        enum { MAXSITES = 64 };
        char names[MAXSITES][64];
        long counts[MAXSITES];
        long bytes[MAXSITES];
        int n = 0;
        for (Block *b = live; b; b = b->next) {
            int i = 0;
            while (i < n && strcmp(names[i], b->site) != 0) {
                i++;
            }
            if (i == n && n < MAXSITES) {
                strcpy(names[n], b->site);
                counts[n] = 0;
                bytes[n] = 0;
                n++;
            }
            if (i < n) {
                counts[i]++;
                bytes[i] += (long)b->size;
            }
        }
        for (int k = 0; k < n; k++) {
            int best = k;
            for (int j = k + 1; j < n; j++) {
                if (bytes[j] > bytes[best]) {
                    best = j;
                }
            }
            if (best != k) {
                char tn[64];
                strcpy(tn, names[k]);
                strcpy(names[k], names[best]);
                strcpy(names[best], tn);
                long t = counts[k]; counts[k] = counts[best]; counts[best] = t;
                t = bytes[k]; bytes[k] = bytes[best]; bytes[best] = t;
            }
            fprintf(stderr, "HIST %-12s %8ld blocks %10ld bytes\n", names[k], counts[k], bytes[k]);
        }
    }
    while (live) {
        const char *s = live->site;
        int ephemeral = !strict && s && (
            strcmp(s, "concat") == 0 || strcmp(s, "strdup") == 0 ||
            strcmp(s, "str_slice") == 0 || strcmp(s, "read_file") == 0 ||
            strcmp(s, "tmp_path") == 0 || strcmp(s, "vec_new") == 0 ||
            strcmp(s, "getenv") == 0 || strcmp(s, "append") == 0 ||
            strcmp(s, "ir") == 0 || strcmp(s, "map") == 0 ||
            strcmp(s, "lex") == 0 ||
            /* struct boxes that the selfhost AST keeps until exit (items of a module) */
            strcmp(s, "struct_new") == 0);
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

int sal_instrument_free_owned(void *p) {
    (void)p;
    return 0;
}

int sal_instrument_block_info(const void *p, int64_t *size, int *place) {
    (void)p;
    (void)size;
    (void)place;
    return 0;
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

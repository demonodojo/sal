#define _POSIX_C_SOURCE 200809L
#include "sal_runtime.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/resource.h>
#include <sys/stat.h>

#if defined(__linux__) || defined(__APPLE__)
#include <sys/stat.h>
static void chmod_dst_exec(const char *dst) {
    chmod(dst, 0755);
}
#endif

static int64_t g_argc = 0;
static const char **g_argv = NULL;

#ifdef SAL_INSTRUMENT
static void *sal_xmalloc(size_t n, const char *site) {
    void *p = sal_instrument_malloc((int64_t)n, 0, site);
    return p;
}
static void sal_xfree(void *p) {
    sal_instrument_free(p);
}
#else
/* One compile allocates hundreds of thousands of tiny strings and vectors.
 * A bump arena makes those allocations a pointer add. Freed blocks stay live
 * until the process exits; the compiler does not reuse them.
 */
typedef struct SalArena {
    struct SalArena *next;
    size_t used;
    size_t cap;
    char data[];
} SalArena;

static SalArena *g_arena;

static void *sal_xmalloc(size_t n, const char *site) {
    (void)site;
    if (n == 0) {
        n = 1;
    }
    n = (n + 7u) & ~7u;
    if (!g_arena || g_arena->used + n > g_arena->cap) {
        size_t cap = (size_t)1 << 20;
        if (n > cap) {
            cap = n;
        }
        SalArena *b = (SalArena *)malloc(sizeof(SalArena) + cap);
        if (!b) {
            return NULL;
        }
        b->next = g_arena;
        b->used = 0;
        b->cap = cap;
        g_arena = b;
    }
    void *p = g_arena->data + g_arena->used;
    g_arena->used += n;
    return p;
}
static void sal_xfree(void *p) {
    (void)p;
}
#endif

/* Heap strings remember len and spare capacity. `str_char` on a source buffer
 * used to call strlen on every byte (quadratic). Literals are not in the table:
 * they are short and strlen is fine. The hot slot is only a table pointer, never
 * a stack temporary, so a reused stack address cannot report a stale length.
 */
typedef struct {
    const char *ptr; /* NULL empty, SAL_STR_TOMB deleted */
    size_t len;
    size_t cap;
} SalStrSlot;

#define SAL_STR_TOMB ((const char *)(uintptr_t)1)

static SalStrSlot *g_str_slots;
static size_t g_str_nslots;
static size_t g_str_live;
static size_t g_str_fill;
static const char *g_str_hot_ptr;
static size_t g_str_hot_len;

static size_t sal_str_slot_hash(const char *p) {
    uintptr_t x = (uintptr_t)p >> 4;
    x *= 0x9E3779B97F4A7C15ULL;
    return (size_t)x;
}

static SalStrSlot *sal_str_find(const char *p) {
    if (!p || !g_str_nslots || p == SAL_STR_TOMB) {
        return NULL;
    }
    size_t mask = g_str_nslots - 1;
    size_t i = sal_str_slot_hash(p) & mask;
    for (size_t n = 0; n < g_str_nslots; n++) {
        const char *k = g_str_slots[i].ptr;
        if (k == NULL) {
            return NULL;
        }
        if (k == p) {
            return &g_str_slots[i];
        }
        i = (i + 1) & mask;
    }
    return NULL;
}

static void sal_str_insert_new(const char *p, size_t len, size_t cap) {
    size_t mask = g_str_nslots - 1;
    size_t i = sal_str_slot_hash(p) & mask;
    for (;;) {
        const char *k = g_str_slots[i].ptr;
        if (k == NULL || k == SAL_STR_TOMB) {
            if (k == NULL) {
                g_str_fill++;
            }
            g_str_slots[i].ptr = p;
            g_str_slots[i].len = len;
            g_str_slots[i].cap = cap;
            g_str_live++;
            return;
        }
        if (k == p) {
            g_str_slots[i].len = len;
            g_str_slots[i].cap = cap;
            return;
        }
        i = (i + 1) & mask;
    }
}

static void sal_str_rehash(size_t nslots) {
    SalStrSlot *old = g_str_slots;
    size_t oldn = g_str_nslots;
    SalStrSlot *neu = (SalStrSlot *)calloc(nslots, sizeof(SalStrSlot));
    if (!neu) {
        return;
    }
    g_str_slots = neu;
    g_str_nslots = nslots;
    g_str_live = 0;
    g_str_fill = 0;
    for (size_t i = 0; i < oldn; i++) {
        const char *k = old[i].ptr;
        if (k && k != SAL_STR_TOMB) {
            sal_str_insert_new(k, old[i].len, old[i].cap);
        }
    }
    free(old);
}

static void sal_str_remember(const char *p, size_t len, size_t cap) {
    if (!p) {
        return;
    }
    if (g_str_nslots == 0 || g_str_fill * 10 >= g_str_nslots * 7) {
        sal_str_rehash(g_str_nslots ? g_str_nslots * 2 : 1024);
    }
    if (!g_str_nslots || g_str_live == g_str_nslots) {
        return;
    }
    SalStrSlot *slot = sal_str_find(p);
    if (slot) {
        slot->len = len;
        slot->cap = cap;
    } else {
        sal_str_insert_new(p, len, cap);
    }
    if (g_str_hot_ptr == p) {
        g_str_hot_len = len;
    }
}

static void sal_str_forget(const char *p) {
    if (!p || !g_str_nslots) {
        if (p && p == g_str_hot_ptr) {
            g_str_hot_ptr = NULL;
        }
        return;
    }
    size_t mask = g_str_nslots - 1;
    size_t i = sal_str_slot_hash(p) & mask;
    for (size_t n = 0; n < g_str_nslots; n++) {
        const char *k = g_str_slots[i].ptr;
        if (k == NULL) {
            break;
        }
        if (k == p) {
            g_str_slots[i].ptr = SAL_STR_TOMB;
            g_str_slots[i].len = 0;
            g_str_slots[i].cap = 0;
            if (g_str_live) {
                g_str_live--;
            }
            break;
        }
        i = (i + 1) & mask;
    }
    if (g_str_hot_ptr == p) {
        g_str_hot_ptr = NULL;
    }
}

static size_t sal_cstr_len(const char *s) {
    if (!s) {
        return 0;
    }
    if (s == g_str_hot_ptr) {
        return g_str_hot_len;
    }
    SalStrSlot *slot = sal_str_find(s);
    if (slot) {
        g_str_hot_ptr = s;
        g_str_hot_len = slot->len;
        return slot->len;
    }
    return strlen(s);
}

void sal_runtime_init(int64_t argc, const char **argv) {
    g_argc = argc;
    g_argv = argv;
    /* Selfhost uses deep recursion (lexer/parser); raise soft stack limit. */
    {
        struct rlimit rl;
        if (getrlimit(RLIMIT_STACK, &rl) == 0) {
            rl.rlim_cur = rl.rlim_max;
            if (rl.rlim_cur < (rlim_t)64 * 1024 * 1024) {
                rl.rlim_cur = (rlim_t)64 * 1024 * 1024;
                if (rl.rlim_cur > rl.rlim_max) {
                    rl.rlim_cur = rl.rlim_max;
                }
            }
            setrlimit(RLIMIT_STACK, &rl);
        }
    }
}

int64_t sal_argc(void) {
    return g_argc;
}

char *sal_argv(int64_t i) {
    if (i < 0 || i >= g_argc || !g_argv || !g_argv[i]) {
        return sal_strdup("");
    }
    return sal_strdup(g_argv[i]);
}

char *sal_strdup(const char *s) {
    if (!s) {
        s = "";
    }
    size_t n = sal_cstr_len(s);
    char *out = (char *)sal_xmalloc(n + 1, "strdup");
    if (!out) {
        return NULL;
    }
    memcpy(out, s, n + 1);
    sal_str_remember(out, n, n + 1);
    return out;
}

void sal_free(void *p) {
    if (p) {
        sal_str_forget(p);
        sal_xfree(p);
    }
}

char *sal_read_file(const char *path) {
    if (!path) {
        return sal_strdup("");
    }
    FILE *f = fopen(path, "rb");
    if (!f) {
        return sal_strdup("");
    }
    if (fseek(f, 0, SEEK_END) != 0) {
        fclose(f);
        return sal_strdup("");
    }
    long sz = ftell(f);
    if (sz < 0) {
        fclose(f);
        return sal_strdup("");
    }
    if (fseek(f, 0, SEEK_SET) != 0) {
        fclose(f);
        return sal_strdup("");
    }
    char *buf = (char *)sal_xmalloc((size_t)sz + 1, "read_file");
    if (!buf) {
        fclose(f);
        return NULL;
    }
    size_t n = fread(buf, 1, (size_t)sz, f);
    fclose(f);
    buf[n] = '\0';
    sal_str_remember(buf, n, (size_t)sz + 1);
    return buf;
}

int64_t sal_write_file(const char *path, const char *data) {
    const char *bytes = data ? data : "";
    FILE *f;
    if (!path || path[0] == '\0' || (path[0] == '-' && path[1] == '\0')) {
        f = stdout;
    } else {
        f = fopen(path, "wb");
        if (!f) {
            return 1;
        }
    }
    size_t n = strlen(bytes);
    size_t w = fwrite(bytes, 1, n, f);
    if (f != stdout) {
        fclose(f);
    } else {
        fflush(stdout);
    }
    return w == n ? 0 : 1;
}

int64_t sal_print_str(const char *data) {
    return sal_write_file("-", data);
}

int64_t sal_eprint_str(const char *data) {
    const char *bytes = data ? data : "";
    size_t n = strlen(bytes);
    size_t w = fwrite(bytes, 1, n, stderr);
    fflush(stderr);
    return w == n ? 0 : 1;
}

char *sal_getenv(const char *name) {
    if (!name) {
        return sal_strdup("");
    }
    const char *v = getenv(name);
    return sal_strdup(v ? v : "");
}

int64_t sal_mkdir_p(const char *path) {
    if (!path || path[0] == '\0') {
        return 1;
    }
    char *tmp = sal_strdup(path);
    if (!tmp) {
        return 1;
    }
    size_t n = strlen(tmp);
    for (size_t i = 1; i < n; i++) {
        if (tmp[i] == '/') {
            tmp[i] = '\0';
            if (tmp[0] != '\0') {
                mkdir(tmp, 0755);
            }
            tmp[i] = '/';
        }
    }
    int rc = 0;
    if (mkdir(tmp, 0755) != 0) {
        /* EEXIST is ok; check directory. */
        struct stat st;
        if (stat(tmp, &st) != 0 || !S_ISDIR(st.st_mode)) {
            rc = 1;
        }
    }
    sal_free(tmp);
    return rc;
}

int64_t sal_str_eq(const char *a, const char *b) {
    if (!a) {
        a = "";
    }
    if (!b) {
        b = "";
    }
    return strcmp(a, b) == 0 ? 1 : 0;
}

int64_t sal_str_contains(const char *hay, const char *needle) {
    if (!hay) {
        hay = "";
    }
    if (!needle || needle[0] == '\0') {
        return 1;
    }
    return strstr(hay, needle) != NULL ? 1 : 0;
}

int64_t sal_str_len(const char *s) {
    return (int64_t)sal_cstr_len(s);
}

char *sal_str_concat(const char *a, const char *b) {
    if (!a) {
        a = "";
    }
    if (!b) {
        b = "";
    }
    size_t na = sal_cstr_len(a);
    size_t nb = sal_cstr_len(b);
    char *out = (char *)sal_xmalloc(na + nb + 1, "concat");
    if (!out) {
        return NULL;
    }
    memcpy(out, a, na);
    memcpy(out + na, b, nb + 1);
    sal_str_remember(out, na + nb, na + nb + 1);
    return out;
}

char *sal_str_append(char *a, const char *b) {
    if (!b) {
        b = "";
    }
    if (!a) {
        return sal_strdup(b);
    }
    size_t nb = sal_cstr_len(b);
    SalStrSlot *slot = sal_str_find(a);
    size_t na = slot ? slot->len : sal_cstr_len(a);
    size_t need = na + nb + 1;
    int overlap = slot && b >= a && b < a + slot->cap;
    if (slot && slot->cap >= need && !overlap) {
        memcpy(a + na, b, nb + 1);
        slot->len = na + nb;
        if (g_str_hot_ptr == a) {
            g_str_hot_len = slot->len;
        }
        return a;
    }
    size_t cap = slot && slot->cap > 16 ? slot->cap : 16;
    while (cap < need) {
        if (cap > SIZE_MAX / 2) {
            cap = need;
            break;
        }
        cap *= 2;
    }
    char *out = (char *)sal_xmalloc(cap, "append");
    if (!out) {
        return NULL;
    }
    memcpy(out, a, na);
    memcpy(out + na, b, nb + 1);
    sal_str_remember(out, na + nb, cap);
    sal_free(a);
    return out;
}

char *sal_select_str(int64_t cond, char *a, char *b) {
    if (cond) {
        sal_free(b);
        return a ? a : sal_strdup("");
    }
    sal_free(a);
    return b ? b : sal_strdup("");
}

int64_t sal_copy_file(const char *src, const char *dst) {
    if (!src || !dst) {
        return 1;
    }
    /* Prefer sendfile-free portable copy; also works for copying the running binary. */
    FILE *in = fopen(src, "rb");
    if (!in) {
        return 1;
    }
    FILE *out = fopen(dst, "wb");
    if (!out) {
        fclose(in);
        return 1;
    }
    char buf[8192];
    size_t n;
    while ((n = fread(buf, 1, sizeof(buf), in)) > 0) {
        if (fwrite(buf, 1, n, out) != n) {
            fclose(in);
            fclose(out);
            return 1;
        }
    }
    fclose(in);
    fclose(out);
#if defined(__linux__) || defined(__APPLE__)
    chmod_dst_exec(dst);
#endif
    return 0;
}

int64_t sal_copy_self(const char *dst) {
    if (!dst) {
        return 1;
    }
#ifdef __linux__
    if (sal_copy_file("/proc/self/exe", dst) == 0) {
        return 0;
    }
#endif
    if (g_argv && g_argc > 0 && g_argv[0]) {
        return sal_copy_file(g_argv[0], dst);
    }
    return 1;
}

int64_t sal_not(int64_t x) {
    return x ? 0 : 1;
}

int64_t sal_gated_print_str(int64_t cond, const char *data) {
    if (!cond) {
        return 0;
    }
    return sal_print_str(data);
}

int64_t sal_gated_copy_self(int64_t cond, const char *dst) {
    if (!cond) {
        return 0;
    }
    return sal_copy_self(dst);
}

int64_t sal_str_char(const char *s, int64_t i) {
    if (!s || i < 0) {
        return 0;
    }
    size_t n = sal_cstr_len(s);
    if ((size_t)i >= n) {
        return 0;
    }
    return (unsigned char)s[i];
}

/* Bulk scans for the selfhost lexer. Kinds match the sal predicates:
 * 1 space, 2 ident char, 3 digit, 4 comment (not newline),
 * 5 preprocess text (stop before quote or ?), 6 preprocess string,
 * 7 string end, 8 space/tab/cr.
 * limit <= 0 means no step cap, except kinds 5 and 6 where the sal
 * scanner returns immediately.
 */
int64_t sal_str_skip(const char *s, int64_t i, int64_t kind, int64_t limit) {
    if (!s || i < 0) {
        return 0;
    }
    size_t n = sal_cstr_len(s);
    if ((size_t)i >= n) {
        return i;
    }
    size_t pos = (size_t)i;
    if (kind == 6) {
        if (limit <= 0) {
            return i;
        }
        size_t left = (size_t)limit;
        while (pos < n && left > 0) {
            unsigned char c = (unsigned char)s[pos];
            if (c == '\\') {
                if (pos + 1 < n) {
                    pos += 2;
                    left--;
                    continue;
                }
                return (int64_t)n;
            }
            if (c == '"') {
                return (int64_t)pos;
            }
            pos++;
            left--;
        }
        return (int64_t)pos;
    }
    if (kind == 7) {
        while (pos < n) {
            unsigned char c = (unsigned char)s[pos];
            if (c == '"') {
                return (int64_t)pos;
            }
            if (c == '\\') {
                pos += 2;
                if (pos >= n) {
                    return (int64_t)pos;
                }
                continue;
            }
            pos++;
        }
        return (int64_t)pos;
    }
    if ((kind == 5) && limit <= 0) {
        return i;
    }
    size_t left = limit > 0 ? (size_t)limit : (n - pos);
    while (pos < n && left > 0) {
        unsigned char c = (unsigned char)s[pos];
        int go = 0;
        if (kind == 1) {
            go = c == ' ';
        } else if (kind == 2) {
            go = (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || c == '_' ||
                 (c >= '0' && c <= '9');
        } else if (kind == 3) {
            go = c >= '0' && c <= '9';
        } else if (kind == 4) {
            go = c != '\n';
        } else if (kind == 5) {
            go = c != '"' && c != '?';
        } else if (kind == 8) {
            go = c == ' ' || c == '\t' || c == '\r';
        }
        if (!go) {
            break;
        }
        pos++;
        left--;
    }
    return (int64_t)pos;
}

int64_t sal_str_hash(const char *s, int64_t seed) {
    size_t n = sal_cstr_len(s);
    uint64_t h = (uint64_t)seed;
    if (!s) {
        return seed;
    }
    for (size_t i = 0; i < n; i++) {
        h = h * 33u + (unsigned char)s[i];
    }
    return (int64_t)h;
}

char *sal_str_slice(const char *s, int64_t start, int64_t end) {
    if (!s) {
        s = "";
    }
    size_t n = sal_cstr_len(s);
    if (start < 0) {
        start = 0;
    }
    if (end < start) {
        end = start;
    }
    if ((size_t)start > n) {
        start = (int64_t)n;
    }
    if ((size_t)end > n) {
        end = (int64_t)n;
    }
    size_t len = (size_t)(end - start);
    char *out = (char *)sal_xmalloc(len + 1, "str_slice");
    if (!out) {
        return NULL;
    }
    memcpy(out, s + start, len);
    out[len] = '\0';
    sal_str_remember(out, len, len + 1);
    return out;
}

char *sal_int_to_str(int64_t v) {
    char buf[32];
    snprintf(buf, sizeof(buf), "%lld", (long long)v);
    return sal_strdup(buf);
}

char *sal_char_to_str(int64_t c) {
    char buf[2];
    buf[0] = (char)(unsigned char)c;
    buf[1] = '\0';
    return sal_strdup(buf);
}

typedef struct {
    int64_t *data;
    size_t len;
    size_t cap;
} SalVec;

void *sal_vec_new(void) {
    SalVec *v = (SalVec *)sal_xmalloc(sizeof(SalVec), "vec_new");
    if (!v) {
        return NULL;
    }
    v->data = NULL;
    v->len = 0;
    v->cap = 0;
    return v;
}

int64_t sal_vec_push(void *vp, int64_t x) {
    SalVec *v = (SalVec *)vp;
    if (!v) {
        return 1;
    }
    if (v->len + 1 > v->cap) {
        size_t ncap = v->cap ? v->cap * 2 : 8;
#ifdef SAL_INSTRUMENT
        int64_t *nd = (int64_t *)realloc(v->data, ncap * sizeof(int64_t));
        if (!nd) {
            sal_panic("oom");
        }
#else
        int64_t *nd = (int64_t *)sal_xmalloc(ncap * sizeof(int64_t), "vec");
        if (!nd) {
            sal_panic("oom");
        }
        if (v->data && v->len) {
            memcpy(nd, v->data, v->len * sizeof(int64_t));
        }
#endif
        v->data = nd;
        v->cap = ncap;
    }
    v->data[v->len++] = x;
    return 0;
}

int64_t sal_vec_get(void *vp, int64_t i) {
    SalVec *v = (SalVec *)vp;
    if (!v || i < 0 || (size_t)i >= v->len) {
        return 0;
    }
    return v->data[i];
}

int64_t sal_vec_set(void *vp, int64_t i, int64_t x) {
    SalVec *v = (SalVec *)vp;
    if (!v || i < 0 || (size_t)i >= v->len) {
        return 1;
    }
    v->data[i] = x;
    return 0;
}

int64_t sal_vec_len(void *vp) {
    SalVec *v = (SalVec *)vp;
    return v ? (int64_t)v->len : 0;
}

/* Open-addressed string map. Keys are borrowed; the compiler's arena keeps them. */
typedef struct {
    const char *key;
    int64_t val;
} SalMapSlot;

typedef struct {
    SalMapSlot *slots;
    size_t nslots;
    size_t fill;
} SalMap;

static size_t sal_map_hash(const char *s) {
    size_t h = 5381;
    if (!s) {
        return h;
    }
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        h = h * 33u + *p;
    }
    return h;
}

static void sal_map_rehash(SalMap *m, size_t nslots) {
    SalMapSlot *ns = (SalMapSlot *)sal_xmalloc(nslots * sizeof(SalMapSlot), "map");
    if (!ns) {
        return;
    }
    memset(ns, 0, nslots * sizeof(SalMapSlot));
    size_t mask = nslots - 1;
    if (m->slots) {
        for (size_t i = 0; i < m->nslots; i++) {
            if (!m->slots[i].key) {
                continue;
            }
            size_t j = sal_map_hash(m->slots[i].key) & mask;
            while (ns[j].key) {
                j = (j + 1) & mask;
            }
            ns[j] = m->slots[i];
        }
        sal_xfree(m->slots);
    }
    m->slots = ns;
    m->nslots = nslots;
}

void *sal_map_new(void) {
    SalMap *m = (SalMap *)sal_xmalloc(sizeof(SalMap), "map");
    if (!m) {
        return NULL;
    }
    m->slots = NULL;
    m->nslots = 0;
    m->fill = 0;
    sal_map_rehash(m, 16);
    return m;
}

int64_t sal_map_get(void *mp, const char *key) {
    SalMap *m = (SalMap *)mp;
    if (!m || !m->nslots || !key) {
        return 0;
    }
    size_t mask = m->nslots - 1;
    size_t i = sal_map_hash(key) & mask;
    for (size_t n = 0; n < m->nslots; n++) {
        const char *k = m->slots[i].key;
        if (!k) {
            return 0;
        }
        if (strcmp(k, key) == 0) {
            return m->slots[i].val;
        }
        i = (i + 1) & mask;
    }
    return 0;
}

int64_t sal_map_put(void *mp, const char *key, int64_t val) {
    SalMap *m = (SalMap *)mp;
    if (!m) {
        return 1;
    }
    if (!key) {
        key = "";
    }
    if (m->nslots == 0 || m->fill * 10 >= m->nslots * 7) {
        size_t nslots = m->nslots ? m->nslots * 2 : 16;
        sal_map_rehash(m, nslots);
    }
    if (!m->nslots) {
        return 1;
    }
    size_t mask = m->nslots - 1;
    size_t i = sal_map_hash(key) & mask;
    for (;;) {
        if (!m->slots[i].key) {
            m->slots[i].key = key;
            m->slots[i].val = val;
            m->fill++;
            return 0;
        }
        if (strcmp(m->slots[i].key, key) == 0) {
            m->slots[i].val = val;
            return 0;
        }
        i = (i + 1) & mask;
    }
}

/* IR text. Layout matches selfhost/main.sal ir_* / region_new / fo_new. */
typedef struct {
    char *p;
    size_t n;
    size_t cap;
} IrBuf;

static void ir_reserve(IrBuf *b, size_t extra) {
    size_t need = b->n + extra + 1;
    if (need <= b->cap) {
        return;
    }
    size_t cap = b->cap ? b->cap : 4096;
    while (cap < need) {
        cap *= 2;
    }
    char *np = (char *)sal_xmalloc(cap, "ir");
    if (!np) {
        return;
    }
    if (b->p && b->n) {
        memcpy(np, b->p, b->n);
    }
    sal_xfree(b->p);
    b->p = np;
    b->cap = cap;
}

static void ir_put(IrBuf *b, const char *s, size_t n) {
    ir_reserve(b, n);
    if (!b->p || b->n + n + 1 > b->cap) {
        return;
    }
    memcpy(b->p + b->n, s, n);
    b->n += n;
    b->p[b->n] = 0;
}

static void ir_puts(IrBuf *b, const char *s) {
    if (!s) {
        s = "";
    }
    ir_put(b, s, strlen(s));
}

static void ir_putc(IrBuf *b, char c) {
    ir_put(b, &c, 1);
}

static void ir_i64(IrBuf *b, int64_t v) {
    char tmp[32];
    snprintf(tmp, sizeof(tmp), "%lld", (long long)v);
    ir_puts(b, tmp);
}

static const char *ir_cstr(int64_t v) {
    if (!v) {
        return "";
    }
    return (const char *)(uintptr_t)v;
}

static SalVec *ir_vec(int64_t v) {
    return (SalVec *)(uintptr_t)v;
}

static int64_t ir_at(SalVec *v, size_t i) {
    if (!v || i >= v->len) {
        return 0;
    }
    return v->data[i];
}

static void ir_quote(IrBuf *b, const char *s) {
    ir_putc(b, '"');
    ir_puts(b, s);
    ir_putc(b, '"');
}

static void ir_quote_esc(IrBuf *b, const char *s) {
    ir_putc(b, '"');
    if (!s) {
        s = "";
    }
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        if (*p == '"') {
            ir_puts(b, "\\\"");
        } else if (*p == '\\') {
            ir_puts(b, "\\\\");
        } else {
            ir_putc(b, (char)*p);
        }
    }
    ir_putc(b, '"');
}

static void ir_emit_inst(IrBuf *b, SalVec *inst);

static void ir_if_piece(IrBuf *parent, SalVec *inst) {
    IrBuf tmp = {0};
    ir_emit_inst(&tmp, inst);
    size_t i = 0;
    size_t end = tmp.n;
    if (tmp.p) {
        while (i < end && tmp.p[i] == ' ') {
            i++;
        }
        if (end > i && tmp.p[end - 1] == '\n') {
            end--;
        }
        ir_put(parent, tmp.p + i, end - i);
    }
    sal_xfree(tmp.p);
}

static void ir_if_body(IrBuf *b, SalVec *body) {
    if (!body) {
        return;
    }
    for (size_t i = 0; i < body->len; i++) {
        if (i > 0) {
            ir_puts(b, ", ");
        }
        ir_if_piece(b, ir_vec(body->data[i]));
    }
}

static void ir_emit_inst(IrBuf *b, SalVec *inst) {
    if (!inst || inst->len == 0) {
        return;
    }
    int64_t k = inst->data[0];
    if (k == 0) {
        ir_puts(b, "  ConstInt {{ dest: ");
        ir_quote(b, ir_cstr(ir_at(inst, 1)));
        ir_puts(b, ", value: ");
        ir_i64(b, ir_at(inst, 2));
        ir_puts(b, " }}\n");
    } else if (k == 1) {
        ir_puts(b, "  ConstString {{ dest: ");
        ir_quote(b, ir_cstr(ir_at(inst, 1)));
        ir_puts(b, ", value: ");
        ir_quote(b, ir_cstr(ir_at(inst, 2)));
        ir_puts(b, " }}\n");
    } else if (k == 2) {
        ir_puts(b, "  Binary {{ dest: ");
        ir_quote(b, ir_cstr(ir_at(inst, 1)));
        ir_puts(b, ", op: ");
        ir_quote(b, ir_cstr(ir_at(inst, 2)));
        ir_puts(b, ", left: ");
        ir_quote(b, ir_cstr(ir_at(inst, 3)));
        ir_puts(b, ", right: ");
        ir_quote(b, ir_cstr(ir_at(inst, 4)));
        ir_puts(b, " }}\n");
    } else if (k == 3) {
        ir_puts(b, "  Call {{ dest: ");
        if (ir_at(inst, 4) == 1) {
            ir_puts(b, "Some(");
            ir_quote(b, ir_cstr(ir_at(inst, 1)));
            ir_putc(b, ')');
        } else {
            ir_puts(b, "None");
        }
        ir_puts(b, ", func: ");
        ir_quote(b, ir_cstr(ir_at(inst, 2)));
        ir_puts(b, ", args: [");
        SalVec *args = ir_vec(ir_at(inst, 3));
        if (args) {
            for (size_t i = 0; i < args->len; i++) {
                if (i > 0) {
                    ir_puts(b, ", ");
                }
                ir_quote_esc(b, ir_cstr(args->data[i]));
            }
        }
        ir_puts(b, "] }}\n");
    } else if (k == 4) {
        ir_puts(b, "  PlaceCopy {{ dest: ");
        ir_quote(b, ir_cstr(ir_at(inst, 1)));
        ir_puts(b, ", from: ");
        ir_quote(b, ir_cstr(ir_at(inst, 2)));
        ir_puts(b, ", to_place: ");
        ir_quote(b, ir_cstr(ir_at(inst, 3)));
        ir_puts(b, " }}\n");
    } else if (k == 5) {
        ir_puts(b, "  Return {{ value: ");
        ir_quote(b, ir_cstr(ir_at(inst, 1)));
        ir_puts(b, " }}\n");
    } else if (k == 6) {
        ir_puts(b, "  Drop {{ name: ");
        ir_quote(b, ir_cstr(ir_at(inst, 1)));
        ir_puts(b, " }}\n");
    } else if (k == 7) {
        ir_puts(b, "  If {{ cond: ");
        ir_quote(b, ir_cstr(ir_at(inst, 2)));
        ir_puts(b, ", then_body: [");
        ir_if_body(b, ir_vec(ir_at(inst, 3)));
        ir_puts(b, "], else_body: [");
        ir_if_body(b, ir_vec(ir_at(inst, 4)));
        ir_puts(b, "], then_val: ");
        ir_quote(b, ir_cstr(ir_at(inst, 5)));
        ir_puts(b, ", else_val: ");
        ir_quote(b, ir_cstr(ir_at(inst, 6)));
        ir_puts(b, ", dest: ");
        ir_quote(b, ir_cstr(ir_at(inst, 1)));
        ir_puts(b, " }}\n");
    }
}

static void ir_emit_op(IrBuf *b, SalVec *op) {
    if (!op || op->len == 0) {
        return;
    }
    int64_t k = op->data[0];
    if (k == 0) {
        ir_puts(b, "    Matmul {{ lhs: ");
        ir_quote(b, ir_cstr(ir_at(op, 1)));
        ir_puts(b, ", rhs: ");
        ir_quote(b, ir_cstr(ir_at(op, 2)));
        ir_puts(b, ", dest: ");
        ir_quote(b, ir_cstr(ir_at(op, 3)));
        ir_puts(b, " }}\n");
    } else if (k == 1) {
        ir_puts(b, "    MapEpilogue {{ op: ");
        ir_quote(b, ir_cstr(ir_at(op, 1)));
        ir_puts(b, ", input: ");
        ir_quote(b, ir_cstr(ir_at(op, 2)));
        ir_puts(b, ", dest: ");
        ir_quote(b, ir_cstr(ir_at(op, 3)));
        ir_puts(b, " }}  # fused epilogue, no intermediate buffer\n");
    } else if (k == 2) {
        ir_puts(b, "    Softmax {{ input: ");
        ir_quote(b, ir_cstr(ir_at(op, 1)));
        ir_puts(b, ", dest: ");
        ir_quote(b, ir_cstr(ir_at(op, 3)));
        ir_puts(b, " }}\n");
    }
}

static void ir_emit_region(IrBuf *b, SalVec *r) {
    if (!r) {
        return;
    }
    ir_puts(b, "  region on ");
    ir_puts(b, ir_cstr(ir_at(r, 0)));
    ir_puts(b, " fused=");
    ir_puts(b, ir_at(r, 2) == 1 ? "true" : "false");
    ir_puts(b, " peak_bytes=");
    if (ir_at(r, 3) == 1) {
        ir_puts(b, "{{\"");
        ir_puts(b, ir_cstr(ir_at(r, 0)));
        ir_puts(b, "\": ");
        ir_i64(b, ir_at(r, 5));
        ir_puts(b, "}}");
    } else {
        ir_puts(b, "{{}}");
    }
    if (ir_at(r, 4) == 1) {
        ir_puts(b, " peak_symbolic={{\"");
        ir_puts(b, ir_cstr(ir_at(r, 0)));
        ir_puts(b, "\": \"");
        ir_puts(b, ir_cstr(ir_at(r, 6)));
        ir_puts(b, "\"}}");
    }
    ir_putc(b, '\n');
    SalVec *ops = ir_vec(ir_at(r, 1));
    if (ops) {
        for (size_t i = 0; i < ops->len; i++) {
            ir_emit_op(b, ir_vec(ops->data[i]));
        }
    }
}

static void ir_emit_fn(IrBuf *b, SalVec *fn) {
    if (!fn) {
        return;
    }
    ir_puts(b, "fn ");
    ir_puts(b, ir_cstr(ir_at(fn, 0)));
    ir_puts(b, ":\n");
    SalVec *dps = ir_vec(ir_at(fn, 1));
    if (dps && dps->len > 0) {
        ir_puts(b, "  dim_params=[");
        for (size_t i = 0; i < dps->len; i++) {
            if (i > 0) {
                ir_puts(b, ", ");
            }
            ir_quote(b, ir_cstr(dps->data[i]));
        }
        ir_puts(b, "]\n");
    }
    SalVec *insts = ir_vec(ir_at(fn, 2));
    if (insts) {
        for (size_t i = 0; i < insts->len; i++) {
            ir_emit_inst(b, ir_vec(insts->data[i]));
        }
    }
    SalVec *regions = ir_vec(ir_at(fn, 3));
    if (regions) {
        for (size_t i = 0; i < regions->len; i++) {
            ir_emit_region(b, ir_vec(regions->data[i]));
        }
    }
}

char *sal_ir_text(void *modp) {
    IrBuf b = {0};
    SalVec *mod = (SalVec *)modp;
    if (mod) {
        for (size_t i = 0; i < mod->len; i++) {
            ir_emit_fn(&b, ir_vec(mod->data[i]));
        }
    }
    if (!b.p) {
        b.p = (char *)sal_xmalloc(1, "ir");
        if (!b.p) {
            return NULL;
        }
        b.p[0] = 0;
        b.n = 0;
        b.cap = 1;
    }
    sal_str_remember(b.p, b.n, b.cap);
    return b.p;
}

void sal_vec_free(void *vp) {
    SalVec *v = (SalVec *)vp;
    if (!v) {
        return;
    }
#ifdef SAL_INSTRUMENT
    free(v->data);
    sal_xfree(v);
#else
    /* Vector storage lives in the bump arena. */
    (void)v;
#endif
}

void *sal_image_new(int64_t w, int64_t h) {
    SalVec *v = (SalVec *)sal_vec_new();
    if (!v || w <= 0 || h <= 0 || w > 4096 || h > 4096) {
        return v;
    }
    size_t n = (size_t)w * (size_t)h * 3u;
    int64_t *data = (int64_t *)malloc(n * sizeof(int64_t));
    if (!data) {
        return v;
    }
    for (size_t i = 0; i < n; i++) {
        data[i] = 255;
    }
    v->data = data;
    v->len = n;
    v->cap = n;
    return v;
}

static uint32_t png_crc(const unsigned char *data, size_t n) {
    uint32_t c = 0xffffffffu;
    for (size_t i = 0; i < n; i++) {
        c ^= data[i];
        for (int k = 0; k < 8; k++) {
            uint32_t mask = -(c & 1u);
            c = (c >> 1) ^ (0xedb88320u & mask);
        }
    }
    return c ^ 0xffffffffu;
}

static uint32_t png_adler(const unsigned char *data, size_t n) {
    uint32_t a = 1;
    uint32_t b = 0;
    for (size_t i = 0; i < n; i++) {
        a = (a + data[i]) % 65521u;
        b = (b + a) % 65521u;
    }
    return (b << 16) | a;
}

static void png_u32be(unsigned char *p, uint32_t v) {
    p[0] = (unsigned char)((v >> 24) & 255u);
    p[1] = (unsigned char)((v >> 16) & 255u);
    p[2] = (unsigned char)((v >> 8) & 255u);
    p[3] = (unsigned char)(v & 255u);
}

static int png_append(unsigned char **buf, size_t *len, size_t *cap, const unsigned char *src, size_t n) {
    if (*len + n > *cap) {
        size_t ncap = *cap ? *cap : 256;
        while (*len + n > ncap) {
            ncap *= 2;
        }
        unsigned char *nd = (unsigned char *)realloc(*buf, ncap);
        if (!nd) {
            return 1;
        }
        *buf = nd;
        *cap = ncap;
    }
    memcpy(*buf + *len, src, n);
    *len += n;
    return 0;
}

static int png_chunk(unsigned char **buf, size_t *len, size_t *cap, const char typ[4], const unsigned char *data, size_t n) {
    unsigned char hdr[8];
    png_u32be(hdr, (uint32_t)n);
    memcpy(hdr + 4, typ, 4);
    if (png_append(buf, len, cap, hdr, 8) != 0) {
        return 1;
    }
    if (n && png_append(buf, len, cap, data, n) != 0) {
        return 1;
    }
    size_t crc_n = 4 + n;
    unsigned char *crc_src = (unsigned char *)malloc(crc_n);
    if (!crc_src) {
        return 1;
    }
    memcpy(crc_src, typ, 4);
    if (n) {
        memcpy(crc_src + 4, data, n);
    }
    unsigned char crc[4];
    png_u32be(crc, png_crc(crc_src, crc_n));
    free(crc_src);
    return png_append(buf, len, cap, crc, 4);
}

int64_t sal_write_png(const char *path, int64_t w, int64_t h, void *rgb) {
    SalVec *v = (SalVec *)rgb;
    if (!path || !v || w <= 0 || h <= 0) {
        return 1;
    }
    size_t need = (size_t)w * (size_t)h * 3u;
    if (v->len < need) {
        return 1;
    }
    size_t raw_n = (size_t)h * ((size_t)w * 3u + 1u);
    unsigned char *raw = (unsigned char *)malloc(raw_n);
    if (!raw) {
        return 1;
    }
    size_t o = 0;
    for (int64_t y = 0; y < h; y++) {
        raw[o++] = 0;
        for (int64_t x = 0; x < w; x++) {
            size_t i = ((size_t)y * (size_t)w + (size_t)x) * 3u;
            raw[o++] = (unsigned char)(v->data[i] & 255);
            raw[o++] = (unsigned char)(v->data[i + 1] & 255);
            raw[o++] = (unsigned char)(v->data[i + 2] & 255);
        }
    }
    size_t zcap = raw_n + (raw_n / 65535u + 2u) * 5u + 16u;
    unsigned char *z = (unsigned char *)malloc(zcap);
    if (!z) {
        free(raw);
        return 1;
    }
    size_t zn = 0;
    z[zn++] = 0x78;
    z[zn++] = 0x01;
    size_t off = 0;
    while (off < raw_n) {
        size_t chunk = raw_n - off;
        if (chunk > 65535u) {
            chunk = 65535u;
        }
        int bfinal = (off + chunk == raw_n);
        z[zn++] = bfinal ? 1 : 0;
        z[zn++] = (unsigned char)(chunk & 255u);
        z[zn++] = (unsigned char)((chunk >> 8) & 255u);
        unsigned int nlen = (~(unsigned int)chunk) & 0xffffu;
        z[zn++] = (unsigned char)(nlen & 255u);
        z[zn++] = (unsigned char)((nlen >> 8) & 255u);
        memcpy(z + zn, raw + off, chunk);
        zn += chunk;
        off += chunk;
    }
    uint32_t ad = png_adler(raw, raw_n);
    free(raw);
    unsigned char adb[4];
    png_u32be(adb, ad);
    memcpy(z + zn, adb, 4);
    zn += 4;

    unsigned char *out = NULL;
    size_t olen = 0;
    size_t ocap = 0;
    static const unsigned char sig[8] = {137, 80, 78, 71, 13, 10, 26, 10};
    int rc = 1;
    if (png_append(&out, &olen, &ocap, sig, 8) == 0) {
        unsigned char ihdr[13];
        png_u32be(ihdr, (uint32_t)w);
        png_u32be(ihdr + 4, (uint32_t)h);
        ihdr[8] = 8;
        ihdr[9] = 2;
        ihdr[10] = 0;
        ihdr[11] = 0;
        ihdr[12] = 0;
        if (png_chunk(&out, &olen, &ocap, "IHDR", ihdr, 13) == 0
            && png_chunk(&out, &olen, &ocap, "IDAT", z, zn) == 0
            && png_chunk(&out, &olen, &ocap, "IEND", NULL, 0) == 0) {
            FILE *f = fopen(path, "wb");
            if (f) {
                size_t wr = fwrite(out, 1, olen, f);
                fclose(f);
                rc = wr == olen ? 0 : 1;
            }
        }
    }
    free(z);
    free(out);
    return rc;
}

void *sal_list_new(void) {
    return sal_vec_new();
}

void *sal_list_push(void *vp, int64_t x) {
    if (sal_vec_push(vp, x) != 0) {
        return NULL;
    }
    return vp;
}

int64_t sal_list_len(void *vp) {
    return sal_vec_len(vp);
}

int64_t sal_list_get(void *vp, int64_t i) {
    /* List bounds checks are memory safety, not an --instrument cost. */
    SalVec *v = (SalVec *)vp;
    int64_t len = v ? (int64_t)v->len : 0;
    sal_instrument_check_index(i, len, "list_get");
    if (!v || i < 0 || (size_t)i >= (size_t)len) {
        sal_panic("list_get: out of bounds");
    }
    return v->data[i];
}

static int find_runtime_dir(char *out, size_t n) {
    const char *env = getenv("SAL_RUNTIME");
    if (env && env[0]) {
        snprintf(out, n, "%s", env);
        return 0;
    }
    const char *cands[] = {"runtime", "./runtime", NULL};
    for (int i = 0; cands[i]; i++) {
        char path[512];
        snprintf(path, sizeof(path), "%s/sal_runtime.c", cands[i]);
        if (access(path, R_OK) == 0) {
            snprintf(out, n, "%s", cands[i]);
            return 0;
        }
    }
#ifdef __linux__
    char exe[512];
    ssize_t m = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
    if (m > 0) {
        exe[m] = 0;
        char *slash = strrchr(exe, '/');
        if (slash) {
            *slash = 0;
            char path[1024];
            snprintf(path, sizeof(path), "%s/../runtime/sal_runtime.c", exe);
            if (access(path, R_OK) == 0) {
                snprintf(out, n, "%s/../runtime", exe);
                return 0;
            }
            snprintf(path, sizeof(path), "%s/runtime/sal_runtime.c", exe);
            if (access(path, R_OK) == 0) {
                snprintf(out, n, "%s/runtime", exe);
                return 0;
            }
        }
    }
#endif
    snprintf(out, n, "runtime");
    return 1;
}

int64_t sal_clang(const char *c_path, const char *out_path) {
    if (!c_path || !out_path) {
        return 1;
    }
    char rdir[512];
    find_runtime_dir(rdir, sizeof(rdir));
    char cmd[4096];
    snprintf(cmd, sizeof(cmd),
             "clang -O0 -g -I%s -o %s %s %s/sal_runtime.c %s/kernels.c %s/instrument.c -lm "
             "2>/tmp/sal-selfhost-clang.err",
             rdir, out_path, c_path, rdir, rdir, rdir);
    int rc = system(cmd);
    if (rc != 0) {
        fprintf(stderr, "sal_clang: clang failed (%d)\n", rc);
        FILE *ef = fopen("/tmp/sal-selfhost-clang.err", "r");
        if (ef) {
            char line[512];
            while (fgets(line, sizeof(line), ef)) {
                fputs(line, stderr);
            }
            fclose(ef);
        }
        return 1;
    }
#if defined(__linux__) || defined(__APPLE__)
    chmod_dst_exec(out_path);
#endif
    return 0;
}

char *sal_tmp_path(const char *suffix) {
    char buf[512];
    const char *suf = suffix ? suffix : "tmp";
    snprintf(buf, sizeof(buf), "/tmp/sal-%d-%s", (int)getpid(), suf);
    return sal_strdup(buf);
}


char *sal_exec_capture(const char *bin, const char *arg) {
    if (!bin) {
        return sal_strdup("");
    }
    char cmd[4096];
    const char *a = arg ? arg : "";
    snprintf(cmd, sizeof(cmd), "%s %s", bin, a);
    FILE *fp = popen(cmd, "r");
    if (!fp) {
        return sal_strdup("");
    }
    size_t cap = 4096, len = 0;
    char *buf = (char *)malloc(cap);
    if (!buf) {
        pclose(fp);
        return NULL;
    }
    buf[0] = 0;
    char tmp[1024];
    while (fgets(tmp, sizeof(tmp), fp)) {
        size_t n = strlen(tmp);
        if (len + n + 1 > cap) {
            cap *= 2;
            char *nb = (char *)realloc(buf, cap);
            if (!nb) {
                free(buf);
                pclose(fp);
                return NULL;
            }
            buf = nb;
        }
        memcpy(buf + len, tmp, n);
        len += n;
        buf[len] = 0;
    }
    pclose(fp);
    char *out = sal_strdup(buf);
    free(buf);
    return out;
}

int64_t sal_exec_compile(const char *bin, const char *src, const char *outp) {
    if (!bin || !src || !outp) {
        return 1;
    }
    char cmd[4096];
    snprintf(cmd, sizeof(cmd), "%s %s -o %s", bin, src, outp);
    int rc = system(cmd);
    return rc == 0 ? 0 : 1;
}

int64_t sal_gated_exec_compile(int64_t cond, const char *bin, const char *src, const char *outp) {
    if (!cond) {
        return 0;
    }
    return sal_exec_compile(bin, src, outp);
}

int64_t sal_print_i64(int64_t v) {

    printf("%lld\n", (long long)v);
    return v;
}

void sal_panic(const char *msg) {
    fprintf(stderr, "sal panic: %s\n", msg);
    exit(1);
}

/*
 * .salt little-endian container:
 *   magic[4] = "SALT"
 *   version  u16
 *   elem     u8   (0 = F32 for this loader)
 *   rank     u8
 *   dims     u64[rank]
 *   payload  little-endian elements
 */
void *sal_load_f32(const char *path, int64_t *out_elems) {
    FILE *f = fopen(path, "rb");
    if (!f) {
        return NULL;
    }
    char magic[4];
    if (fread(magic, 1, 4, f) != 4 || memcmp(magic, "SALT", 4) != 0) {
        fclose(f);
        return NULL;
    }
    uint16_t ver = 0;
    uint8_t elem = 0, rank = 0;
    if (fread(&ver, 2, 1, f) != 1 || fread(&elem, 1, 1, f) != 1 || fread(&rank, 1, 1, f) != 1) {
        fclose(f);
        return NULL;
    }
    (void)ver;
    (void)elem; /* 0 = F32; this loader always materializes f32 payload */
    int64_t count = 1;
    for (uint8_t i = 0; i < rank; i++) {
        uint64_t d = 0;
        if (fread(&d, 8, 1, f) != 1) {
            fclose(f);
            return NULL;
        }
        if (d == 0 || count > (INT64_MAX / (int64_t)d)) {
            fclose(f);
            return NULL;
        }
        count *= (int64_t)d;
    }
    float *buf = (float *)malloc((size_t)count * sizeof(float));
    if (!buf) {
        fclose(f);
        return NULL;
    }
    if (fread(buf, sizeof(float), (size_t)count, f) != (size_t)count) {
        free(buf);
        fclose(f);
        return NULL;
    }
    fclose(f);
    if (out_elems) {
        *out_elems = count;
    }
    return buf;
}

/* ---- Per-actor heaps (cpu=0, gpu=1, tpu=2); host malloc backed ---- */

enum { SAL_PLACE_N = 3 };

typedef struct SalPlaceBlock {
    void *ptr;
    int place;
    struct SalPlaceBlock *next;
} SalPlaceBlock;

static SalPlaceBlock *g_place_heaps[SAL_PLACE_N];
static int64_t g_place_launches[SAL_PLACE_N];

static int clamp_place(int place) {
    if (place < 0 || place >= SAL_PLACE_N) {
        return 0;
    }
    return place;
}

void *sal_place_malloc(int64_t size, int place) {
    place = clamp_place(place);
    if (size < 0) {
        size = 0;
    }
    void *p = malloc((size_t)size);
    if (!p) {
        return NULL;
    }
    SalPlaceBlock *b = (SalPlaceBlock *)malloc(sizeof(SalPlaceBlock));
    if (!b) {
        free(p);
        return NULL;
    }
    b->ptr = p;
    b->place = place;
    b->next = g_place_heaps[place];
    g_place_heaps[place] = b;
    return p;
}

void sal_place_free(void *p) {
    if (!p) {
        return;
    }
    for (int pl = 0; pl < SAL_PLACE_N; pl++) {
        SalPlaceBlock **cur = &g_place_heaps[pl];
        while (*cur) {
            if ((*cur)->ptr == p) {
                SalPlaceBlock *dead = *cur;
                *cur = dead->next;
                free(dead->ptr);
                free(dead);
                return;
            }
            cur = &(*cur)->next;
        }
    }
    free(p);
}

void *sal_place_copy(const void *src, int64_t nbytes, int to_place) {
    to_place = clamp_place(to_place);
    if (nbytes < 0) {
        nbytes = 0;
    }
    void *dst = sal_place_malloc(nbytes, to_place);
    if (!dst) {
        return NULL;
    }
    if (src && nbytes > 0) {
        memcpy(dst, src, (size_t)nbytes);
    }
    return dst;
}

void sal_on_enter(int64_t place) {
    int p = clamp_place((int)place);
    g_place_launches[p] += 1;
}

int64_t sal_place_launches(int64_t place) {
    int p = clamp_place((int)place);
    return g_place_launches[p];
}

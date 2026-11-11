/* ASan harness for the String primitives (SPEC «String»: a String never changes once built).
 * Exit 0 on success; a failing check returns its line number. */
#include "../../runtime/sal_runtime.h"

#include <string.h>

/* A literal as the compiler emits it: cap 0, len, bytes (a bare C string has no header). */
#define SAL_LIT(name, text)                       \
    static const struct __attribute__((aligned(16))) { \
        int64_t cap;                              \
        int64_t len;                              \
        char b[sizeof(text)];                     \
    } name##_blk = {0, sizeof(text) - 1, text};   \
    static const char *name = name##_blk.b;

SAL_LIT(g_lit_s, "lit")
SAL_LIT(g_empty_s, "")
SAL_LIT(g_dash, "-")
SAL_LIT(g_bang, "!")
SAL_LIT(g_ab, "ab")
SAL_LIT(g_y, "y")

#define CHECK(c)          \
    do {                  \
        if (!(c)) {       \
            return __LINE__; \
        }                 \
    } while (0)

static int same(const char *s, const char *want) {
    return sal_str_len(s) == (int64_t)strlen(want) && strcmp(s, want) == 0;
}

static int run(void) {
    const char *lit = g_lit_s;

    /* concat: neither operand changes, the result is a new block. */
    char *a = sal_strdup(lit);
    char *b = sal_str_concat(a, lit);
    CHECK(same(a, "lit") && same(b, "litlit") && b != a);

    /* concat_n: one block with every part, parts unchanged; literal and empty parts. */
    const char *parts[5] = {a, g_empty_s, b, lit, NULL};
    char *c = sal_str_concat_n(5, parts);
    CHECK(same(c, "litlitlitlit") && same(a, "lit") && same(b, "litlit"));
    const char *none[2] = {NULL, NULL};
    char *e = sal_str_concat_n(2, none);
    CHECK(sal_str_len(e) == 0 && e[0] == 0);
    sal_free(e);

    /* append on a literal copies: the literal stays the same. */
    char *d = sal_str_append((char *)lit, g_bang);
    CHECK(same(d, "lit!") && same(lit, "lit"));

    /* append_n reuses its own block when it fits, and the result is correct when a part is
     * the accumulator itself (`x = x + a + x`). */
    char *x = sal_strdup(lit);
    const char *self_parts[2] = {g_dash, x};
    x = sal_str_append_n(x, 2, self_parts);
    CHECK(same(x, "lit-lit"));
    for (int i = 0; i < 1000; i++) {
        const char *more[2] = {g_ab, lit};
        x = sal_str_append_n(x, 2, more);
    }
    CHECK(sal_str_len(x) == 7 + 1000 * 5);
    const char *self2[1] = {x};
    x = sal_str_append_n(x, 1, self2);
    CHECK(sal_str_len(x) == 2 * (7 + 1000 * 5) && strncmp(x, "lit-litablit", 12) == 0);

    /* large parts. */
    char *big = sal_strdup(g_empty_s);
    for (int i = 0; i < 4096; i++) {
        big = sal_str_append(big, g_y);
    }
    const char *bigs[4] = {big, big, big, big};
    char *bb = sal_str_concat_n(4, bigs);
    CHECK(sal_str_len(bb) == 4 * 4096 && sal_str_len(big) == 4096);

    sal_free(a);
    sal_free(b);
    sal_free(c);
    sal_free(d);
    sal_free(x);
    sal_free(big);
    sal_free(bb);
    sal_free((char *)lit); /* a literal is never freed: a no-op */
    return 0;
}

int main(void) {
    return run();
}

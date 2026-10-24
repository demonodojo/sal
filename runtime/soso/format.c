#include "platform.h"
#include "../sal_runtime.h"
/* Only the %s, %d and %lld formats used by the common runtime. */
static size_t format(char *out, size_t cap, const char *fmt, va_list ap) {
    size_t n = 0;
#define PUT(c) do { char ch = (c); if (cap && n < cap - 1) out[n] = ch; n++; } while (0)
    while (*fmt) {
        if (*fmt != '%') { PUT(*fmt++); continue; }
        fmt++;
        if (*fmt == '%') { PUT('%'); fmt++; continue; }
        if (!strncmp(fmt, ".17g", 4)) {
            char num[64]; size_t len = sal_os_format_f64(va_arg(ap, double), num, sizeof(num));
            for (size_t i = 0; i < len; i++) { PUT(num[i]); }
            fmt += 4; continue;
        }
        if (*fmt == 's') {
            const char *s = va_arg(ap, const char *); if (!s) s = "(null)";
            while (*s) { PUT(*s++); } fmt++; continue;
        }
        int wide = 0;
        if (fmt[0] == 'l' && fmt[1] == 'l') { wide = 1; fmt += 2; }
        if (*fmt != 'd') sal_panic("formato no soportado por runtime soso");
        int64_t v = wide ? va_arg(ap, long long) : va_arg(ap, int);
        uint64_t u = (uint64_t)v;
        if (v < 0) { PUT('-'); u = 0 - u; }
        char digits[20]; size_t k = 0;
        do { digits[k++] = '0' + u % 10; u /= 10; } while (u);
        while (k) { PUT(digits[--k]); }
        fmt++;
    }
    if (cap) out[n < cap ? n : cap - 1] = 0;
    return n;
#undef PUT
}
int snprintf(char *out, size_t cap, const char *fmt, ...) {
    va_list ap; va_start(ap, fmt); size_t n = format(out, cap, fmt, ap); va_end(ap); return (int)n;
}
static int print(FILE *f, const char *fmt, va_list ap) {
    va_list copy; va_copy(copy, ap); size_t n = format(NULL, 0, fmt, copy); va_end(copy);
    char *buf = malloc(n + 1); if (!buf) return -1;
    format(buf, n + 1, fmt, ap);
    size_t wrote = fwrite(buf, 1, n, f); free(buf); return wrote == n ? (int)n : -1;
}
int printf(const char *fmt, ...) {
    va_list ap; va_start(ap, fmt); int n = print(stdout, fmt, ap); va_end(ap); return n;
}
int fprintf(FILE *f, const char *fmt, ...) {
    va_list ap; va_start(ap, fmt); int n = print(f, fmt, ap); va_end(ap); return n;
}

#include "sal_runtime.h"

/* Minimal stubs when linking kernels.c without sal_runtime.c */
int sal_mem_place(const void *p) {
    (void)p;
    return 0;
}

void sal_gpu_matmul_f32(const float *a, const float *b, float *out, int64_t m, int64_t k,
                        int64_t n) {
    (void)a;
    (void)b;
    (void)out;
    (void)m;
    (void)k;
    (void)n;
}

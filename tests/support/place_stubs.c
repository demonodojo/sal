#include "sal_runtime.h"

#include <stdlib.h>

/* Minimal stubs when linking kernels.c without sal_runtime.c (GPU via gpu_driver.c). */
int sal_mem_place(const void *p) {
    (void)p;
    return 0;
}

void *sal_arena_realloc(void *old, int64_t old_n, int64_t new_n) {
    (void)old_n;
    return realloc(old, new_n > 0 ? (size_t)new_n : 1);
}

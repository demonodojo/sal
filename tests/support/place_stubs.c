#include "sal_runtime.h"

/* Minimal stubs when linking kernels.c without sal_runtime.c (GPU via gpu_driver.c). */
int sal_mem_place(const void *p) {
    (void)p;
    return 0;
}

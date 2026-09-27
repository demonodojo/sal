#include "sal_runtime.h"

#include <stdlib.h>
#include <string.h>

#ifdef SAL_USE_CUDA
#include <cuda_runtime.h>

static int g_cuda_ready = 0;
static int g_cuda_tried = 0;

static void sal_cuda_init_once(void) {
    if (g_cuda_tried) {
        return;
    }
    g_cuda_tried = 1;
    g_cuda_ready = (cudaSetDevice(0) == cudaSuccess);
}

int sal_gpu_enabled(void) {
    sal_cuda_init_once();
    return g_cuda_ready;
}

void *sal_gpu_alloc(int64_t nbytes) {
    sal_cuda_init_once();
    if (!g_cuda_ready || nbytes <= 0) {
        return NULL;
    }
    void *p = NULL;
    if (cudaMalloc(&p, (size_t)nbytes) != cudaSuccess) {
        return NULL;
    }
    return p;
}

void sal_gpu_free(void *p) {
    if (!p) {
        return;
    }
    if (g_cuda_ready) {
        cudaFree(p);
    }
}

int sal_gpu_copy_h2d(void *dst, const void *src, int64_t nbytes) {
    sal_cuda_init_once();
    if (!g_cuda_ready || nbytes == 0) {
        return -1;
    }
    return cudaMemcpy(dst, src, (size_t)nbytes, cudaMemcpyHostToDevice) == cudaSuccess ? 0 : -1;
}

int sal_gpu_copy_d2h(void *dst, const void *src, int64_t nbytes) {
    sal_cuda_init_once();
    if (!g_cuda_ready || nbytes == 0) {
        return -1;
    }
    return cudaMemcpy(dst, src, (size_t)nbytes, cudaMemcpyDeviceToHost) == cudaSuccess ? 0 : -1;
}

int sal_gpu_copy_d2d(void *dst, const void *src, int64_t nbytes) {
    sal_cuda_init_once();
    if (!g_cuda_ready || nbytes == 0) {
        return -1;
    }
    return cudaMemcpy(dst, src, (size_t)nbytes, cudaMemcpyDeviceToDevice) == cudaSuccess ? 0 : -1;
}

void sal_gpu_sync(void) {
    if (g_cuda_ready) {
        cudaDeviceSynchronize();
    }
}

#else

int sal_gpu_enabled(void) {
    return 0;
}

void *sal_gpu_alloc(int64_t nbytes) {
    (void)nbytes;
    return NULL;
}

void sal_gpu_free(void *p) {
    (void)p;
}

int sal_gpu_copy_h2d(void *dst, const void *src, int64_t nbytes) {
    (void)dst;
    (void)src;
    (void)nbytes;
    return -1;
}

int sal_gpu_copy_d2h(void *dst, const void *src, int64_t nbytes) {
    (void)dst;
    (void)src;
    (void)nbytes;
    return -1;
}

int sal_gpu_copy_d2d(void *dst, const void *src, int64_t nbytes) {
    (void)dst;
    (void)src;
    (void)nbytes;
    return -1;
}

void sal_gpu_sync(void) {
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

#endif

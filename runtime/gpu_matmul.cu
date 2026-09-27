#include "sal_runtime.h"

#include <stdint.h>

#ifdef SAL_USE_CUDA

__global__ void sal_matmul_f32_kernel(const float *a, const float *b, float *out, int m, int k,
                                      int n) {
    int j = (int)(blockIdx.x * blockDim.x + threadIdx.x);
    int i = (int)(blockIdx.y * blockDim.y + threadIdx.y);
    if (i >= m || j >= n) {
        return;
    }
    float sum = 0.f;
    for (int t = 0; t < k; t++) {
        sum += a[i * k + t] * b[t * n + j];
    }
    out[i * n + j] = sum;
}

void sal_gpu_matmul_f32(const float *a, const float *b, float *out, int64_t m, int64_t k,
                        int64_t n) {
    if (m <= 0 || k <= 0 || n <= 0) {
        return;
    }
    dim3 block(16, 16);
    dim3 grid((unsigned)((n + 15) / 16), (unsigned)((m + 15) / 16));
    sal_matmul_f32_kernel<<<grid, block>>>(a, b, out, (int)m, (int)k, (int)n);
    cudaDeviceSynchronize();
}

#else

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

#include "sal_runtime.h"

#include <stdint.h>

#ifdef SAL_USE_CUDA

#ifndef SAL_CUDA_TILE
#define SAL_CUDA_TILE 16
#endif

__global__ void sal_matmul_f32_kernel(const float *a, const float *b, float *out, int m, int k,
                                      int n) {
    __shared__ float tile_a[SAL_CUDA_TILE][SAL_CUDA_TILE];
    __shared__ float tile_b[SAL_CUDA_TILE][SAL_CUDA_TILE];

    int row = (int)(blockIdx.y * blockDim.y + threadIdx.y);
    int col = (int)(blockIdx.x * blockDim.x + threadIdx.x);

    float sum = 0.f;
    for (int t0 = 0; t0 < k; t0 += SAL_CUDA_TILE) {
        int ta_col = t0 + (int)threadIdx.x;
        int tb_row = t0 + (int)threadIdx.y;
        tile_a[(int)threadIdx.y][(int)threadIdx.x] =
            (row < m && ta_col < k) ? a[row * k + ta_col] : 0.f;
        tile_b[(int)threadIdx.y][(int)threadIdx.x] =
            (tb_row < k && col < n) ? b[tb_row * n + col] : 0.f;
        __syncthreads();
        for (int t = 0; t < SAL_CUDA_TILE; t++) {
            sum += tile_a[(int)threadIdx.y][t] * tile_b[t][(int)threadIdx.x];
        }
        __syncthreads();
    }
    if (row < m && col < n) {
        out[row * n + col] = sum;
    }
}

void sal_gpu_matmul_f32(const float *a, const float *b, float *out, int64_t m, int64_t k,
                        int64_t n) {
    if (m <= 0 || k <= 0 || n <= 0) {
        return;
    }
    dim3 block(SAL_CUDA_TILE, SAL_CUDA_TILE);
    dim3 grid((unsigned)((n + SAL_CUDA_TILE - 1) / SAL_CUDA_TILE),
              (unsigned)((m + SAL_CUDA_TILE - 1) / SAL_CUDA_TILE));
    sal_matmul_f32_kernel<<<grid, block>>>(a, b, out, (int)m, (int)k, (int)n);
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

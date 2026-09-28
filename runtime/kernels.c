#include "sal_runtime.h"

#include <math.h>

/* Tiled F32 matmul; same numeric result as a naive triple loop. */
#ifndef SAL_MATMUL_TILE
#define SAL_MATMUL_TILE 32
#endif

void sal_matmul_f32(const float *a, const float *b, float *out, int64_t m, int64_t k, int64_t n) {
    int place = sal_mem_place(a);
    if (place < 0) {
        place = sal_mem_place(out);
    }
    if (place == 1 && sal_gpu_enabled()) {
        sal_gpu_matmul_f32(a, b, out, m, k, n);
        return;
    }
    for (int64_t i0 = 0; i0 < m; i0 += SAL_MATMUL_TILE) {
        int64_t i1 = i0 + SAL_MATMUL_TILE;
        if (i1 > m) {
            i1 = m;
        }
        for (int64_t j0 = 0; j0 < n; j0 += SAL_MATMUL_TILE) {
            int64_t j1 = j0 + SAL_MATMUL_TILE;
            if (j1 > n) {
                j1 = n;
            }
            for (int64_t t0 = 0; t0 < k; t0 += SAL_MATMUL_TILE) {
                int64_t t1 = t0 + SAL_MATMUL_TILE;
                if (t1 > k) {
                    t1 = k;
                }
                for (int64_t i = i0; i < i1; i++) {
                    for (int64_t j = j0; j < j1; j++) {
                        float sum = (t0 == 0) ? 0.f : out[i * n + j];
                        for (int64_t t = t0; t < t1; t++) {
                            sum += a[i * k + t] * b[t * n + j];
                        }
                        out[i * n + j] = sum;
                    }
                }
            }
        }
    }
}

/* Softmax: reduce and normalize in ascending index order (observable). */
static void tensor_bin_f32_host(int op, float *out, const float *a, const float *b, int64_t len,
                                int b_is_scalar, double b_scalar) {
    for (int64_t i = 0; i < len; i++) {
        float av = a[i];
        float bv = b_is_scalar ? (float)b_scalar : b[i];
        switch (op) {
        case 0:
            out[i] = av + bv;
            break;
        case 1:
            out[i] = av - bv;
            break;
        case 2:
            out[i] = av * bv;
            break;
        case 3:
            out[i] = av / bv;
            break;
        default:
            out[i] = 0.f;
            break;
        }
    }
}

void sal_tensor_bin_f32(int op, float *out, const float *a, const float *b, int64_t len,
                        int b_is_scalar, double b_scalar) {
    (void)sal_mem_place;
    tensor_bin_f32_host(op, out, a, b, len, b_is_scalar, b_scalar);
}

void sal_tensor_neg_f32(float *out, const float *a, int64_t len) {
    for (int64_t i = 0; i < len; i++) {
        out[i] = -a[i];
    }
}

void sal_tensor_not_i8(int8_t *out, const int8_t *a, int64_t len) {
    for (int64_t i = 0; i < len; i++) {
        out[i] = (int8_t)(a[i] == 0 ? 1 : 0);
    }
}

static void tensor_cmp_f32_host(int op, int8_t *out, const float *a, const float *b, int64_t len,
                                int b_is_scalar, double b_scalar) {
    for (int64_t i = 0; i < len; i++) {
        float av = a[i];
        float bv = b_is_scalar ? (float)b_scalar : b[i];
        int8_t v = 0;
        switch (op) {
        case 4:
            v = (av == bv) ? 1 : 0;
            break;
        case 5:
            v = (av != bv) ? 1 : 0;
            break;
        case 6:
            v = (av < bv) ? 1 : 0;
            break;
        case 7:
            v = (av <= bv) ? 1 : 0;
            break;
        case 8:
            v = (av > bv) ? 1 : 0;
            break;
        case 9:
            v = (av >= bv) ? 1 : 0;
            break;
        default:
            v = 0;
            break;
        }
        out[i] = v;
    }
}

void sal_tensor_cmp_f32(int op, int8_t *out, const float *a, const float *b, int64_t len,
                        int b_is_scalar, double b_scalar) {
    tensor_cmp_f32_host(op, out, a, b, len, b_is_scalar, b_scalar);
}

void sal_softmax_f32(float *data, int64_t len) {
    if (len <= 0) {
        return;
    }
    float maxv = data[0];
    for (int64_t i = 1; i < len; i++) {
        if (data[i] > maxv) {
            maxv = data[i];
        }
    }
    float sum = 0.f;
    for (int64_t i = 0; i < len; i++) {
        data[i] = expf(data[i] - maxv);
        sum += data[i];
    }
    for (int64_t i = 0; i < len; i++) {
        data[i] /= sum;
    }
}

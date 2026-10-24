#include "sal_runtime.h"

#ifdef SAL_SOSO
#include "soso/platform.h"
#else
#include <math.h>
#include <stdlib.h>
#include <string.h>
#endif

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

int64_t sal_mask_select_count(const int8_t *mask, int64_t len) {
    int64_t n = 0;
    for (int64_t i = 0; i < len; i++) {
        if (mask[i] != 0) {
            n++;
        }
    }
    return n;
}

void sal_tensor_select_f32(float *out, const float *in, const int8_t *mask, int64_t len) {
    int64_t j = 0;
    for (int64_t i = 0; i < len; i++) {
        if (mask[i] != 0) {
            out[j++] = in[i];
        }
    }
}

void sal_tensor_select_i8(int8_t *out, const int8_t *in, const int8_t *mask, int64_t len) {
    int64_t j = 0;
    for (int64_t i = 0; i < len; i++) {
        if (mask[i] != 0) {
            out[j++] = in[i];
        }
    }
}

void sal_tensor_bin_f32(int op, float *out, const float *a, const float *b, int64_t len,
                        int b_is_scalar, double b_scalar) {
    /* GPU heap: host loop today (like matmul without SAL_USE_CUDA). Dedicated CUDA in a later .cu. */
    int place = sal_mem_place(a);
    if (place < 0) {
        place = sal_mem_place(out);
    }
    (void)place;
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
    int place = sal_mem_place(a);
    if (place < 0) {
        place = sal_mem_place(out);
    }
    (void)place;
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

static float half_to_float(uint16_t h) {
    uint32_t s = (uint32_t)(h & 0x8000u) << 16;
    uint32_t e = (h & 0x7C00u) >> 10;
    uint32_t f = (h & 0x03FFu);
    if (e == 0) {
        if (f == 0) {
            union {
                uint32_t u;
                float v;
            } x = {s};
            return x.v;
        }
        e = 1;
        while ((f & 0x0400u) == 0) {
            f <<= 1;
            e--;
        }
        f &= 0x03FFu;
    } else if (e == 31) {
        union {
            uint32_t u;
            float v;
        } x = {s | 0x7F800000u | (f << 13)};
        return x.v;
    }
    e = e + (127 - 15);
    union {
        uint32_t u;
        float v;
    } x = {s | (e << 23) | (f << 13)};
    return x.v;
}

static float bf16_to_float(uint16_t b) {
    union {
        uint32_t u;
        float v;
    } x = {(uint32_t)b << 16};
    return x.v;
}

void sal_matmul_f16(const uint16_t *a, const uint16_t *b, float *out, int64_t m, int64_t k,
                    int64_t n) {
    for (int64_t i = 0; i < m; i++) {
        for (int64_t j = 0; j < n; j++) {
            float sum = 0.f;
            for (int64_t t = 0; t < k; t++) {
                sum += half_to_float(a[i * k + t]) * half_to_float(b[t * n + j]);
            }
            out[i * n + j] = sum;
        }
    }
}

void sal_matmul_bf16(const uint16_t *a, const uint16_t *b, float *out, int64_t m, int64_t k,
                     int64_t n) {
    for (int64_t i = 0; i < m; i++) {
        for (int64_t j = 0; j < n; j++) {
            float sum = 0.f;
            for (int64_t t = 0; t < k; t++) {
                sum += bf16_to_float(a[i * k + t]) * bf16_to_float(b[t * n + j]);
            }
            out[i * n + j] = sum;
        }
    }
}

void sal_softmax_axis_f32(float *data, int64_t outer, int64_t axis_len) {
    if (outer <= 0 || axis_len <= 0) {
        return;
    }
    for (int64_t o = 0; o < outer; o++) {
        float *slice = data + o * axis_len;
        sal_softmax_f32(slice, axis_len);
    }
}

void sal_reduce_axis_f32(const float *in, float *out, int64_t outer, int64_t axis_len) {
    if (outer <= 0 || axis_len <= 0) {
        return;
    }
    for (int64_t o = 0; o < outer; o++) {
        float sum = 0.f;
        for (int64_t i = 0; i < axis_len; i++) {
            sum += in[o * axis_len + i];
        }
        out[o] = sum;
    }
}

void sal_silu_f32(float *out, const float *in, int64_t len) {
    for (int64_t i = 0; i < len; i++) {
        float x = in[i];
        out[i] = x / (1.f + expf(-x));
    }
}

void sal_rms_norm_f32(float *out, const float *in, const float *weight, int64_t len, double eps) {
    if (len <= 0) {
        return;
    }
    double sumsq = 0.0;
    for (int64_t i = 0; i < len; i++) {
        double v = (double)in[i];
        sumsq += v * v;
    }
    double inv = 1.0 / sqrt(sumsq / (double)len + eps);
    for (int64_t i = 0; i < len; i++) {
        out[i] = (float)((double)in[i] * inv * (double)weight[i]);
    }
}

void sal_rope_f32(float *data, int64_t seq, int64_t dim, int64_t pos) {
    if (seq <= 0 || dim < 2 || (dim & 1) != 0) {
        return;
    }
    for (int64_t s = 0; s < seq; s++) {
        int64_t p = pos + s;
        for (int64_t d = 0; d < dim; d += 2) {
            double freq = (double)p / pow(10000.0, (double)d / (double)dim);
            float cosv = (float)cos(freq);
            float sinv = (float)sin(freq);
            float x0 = data[s * dim + d];
            float x1 = data[s * dim + d + 1];
            data[s * dim + d] = x0 * cosv - x1 * sinv;
            data[s * dim + d + 1] = x0 * sinv + x1 * cosv;
        }
    }
}

void sal_attention_f32(float *out, const float *q, const float *k, const float *v, int64_t seq,
                       int64_t heads, int64_t kv_heads, int64_t head_dim, int64_t causal) {
    if (seq <= 0 || heads <= 0 || kv_heads <= 0 || head_dim <= 0) {
        return;
    }
    int64_t h_per_kv = heads / kv_heads;
    for (int64_t h = 0; h < heads; h++) {
        int64_t kv_h = h / h_per_kv;
        for (int64_t si = 0; si < seq; si++) {
            const float *qrow = q + (si * heads + h) * head_dim;
            float maxv = -1e30f;
            float sum = 0.f;
            float acc[512];
            if (head_dim > 512) {
                return;
            }
            int64_t t_end = causal ? si + 1 : seq;
            for (int64_t tj = 0; tj < t_end; tj++) {
                const float *krow = k + (tj * kv_heads + kv_h) * head_dim;
                float dot = 0.f;
                for (int64_t d = 0; d < head_dim; d++) {
                    dot += qrow[d] * krow[d];
                }
                dot *= (float)(1.0 / sqrt((double)head_dim));
                if (tj == 0 || dot > maxv) {
                    maxv = dot;
                }
                acc[tj] = dot;
            }
            for (int64_t tj = 0; tj < t_end; tj++) {
                float e = expf(acc[tj] - maxv);
                acc[tj] = e;
                sum += e;
            }
            float *orow = out + (si * heads + h) * head_dim;
            for (int64_t d = 0; d < head_dim; d++) {
                orow[d] = 0.f;
            }
            for (int64_t tj = 0; tj < t_end; tj++) {
                const float *vrow = v + (tj * kv_heads + kv_h) * head_dim;
                float w = acc[tj] / sum;
                for (int64_t d = 0; d < head_dim; d++) {
                    orow[d] += w * vrow[d];
                }
            }
        }
    }
}

void sal_qmatmul_f32(const float *a, const int8_t *w, const float *scale, float *out, int64_t m,
                     int64_t k, int64_t n, int64_t group) {
    if (group <= 0) {
        group = k;
    }
    for (int64_t i = 0; i < m; i++) {
        for (int64_t j = 0; j < n; j++) {
            float sum = 0.f;
            for (int64_t t = 0; t < k; t++) {
                int64_t g = t / group;
                float sc = scale[g];
                sum += a[i * k + t] * ((float)w[t * n + j] * sc);
            }
            out[i * n + j] = sum;
        }
    }
}

void sal_tensor_write_row_f32(float *cache, int64_t seq, const float *row, int64_t row_elems) {
    if (!cache || !row || row_elems <= 0 || seq < 0) {
        return;
    }
    memcpy(cache + seq * row_elems, row, (size_t)row_elems * sizeof(float));
}

void sal_tensor_write_rows_f32(float *cache, int64_t seq, const float *block, int64_t n_rows,
                               int64_t row_elems) {
    if (!cache || !block || row_elems <= 0 || n_rows <= 0 || seq < 0) {
        return;
    }
    for (int64_t i = 0; i < n_rows; i++) {
        memcpy(cache + (seq + i) * row_elems, block + i * row_elems,
               (size_t)row_elems * sizeof(float));
    }
}

float *sal_tensor_append_f32(float *cache, int64_t *cap, int64_t *len, const float *row,
                              int64_t row_elems) {
    if (!cap || !len || row_elems <= 0) {
        return cache;
    }
    int64_t need = *len + row_elems;
    if (!cache || need > *cap) {
        int64_t new_cap = *cap > 0 ? *cap : row_elems;
        while (new_cap < need) {
            new_cap *= 2;
        }
        float *nbuf = (float *)realloc(cache, (size_t)new_cap * sizeof(float));
        if (!nbuf) {
            return cache;
        }
        cache = nbuf;
        *cap = new_cap;
    }
    memcpy(cache + *len, row, (size_t)row_elems * sizeof(float));
    *len = need;
    return cache;
}

int64_t sal_argmax_axis_f32(const float *data, int64_t len) {
    if (len <= 0) {
        return 0;
    }
    int64_t best = 0;
    float bv = data[0];
    for (int64_t i = 1; i < len; i++) {
        if (data[i] > bv) {
            bv = data[i];
            best = i;
        }
    }
    return best;
}

#ifndef SAL_RUNTIME_H
#define SAL_RUNTIME_H

#include <stdint.h>

int64_t sal_print_i64(int64_t v);
void sal_panic(const char *msg);

void sal_matmul_f32(const float *a, const float *b, float *out, int64_t m, int64_t k, int64_t n);
void sal_gpu_matmul_f32(const float *a, const float *b, float *out, int64_t m, int64_t k,
                        int64_t n);
void sal_softmax_f32(float *data, int64_t len);

int sal_gpu_enabled(void);
void *sal_gpu_alloc(int64_t nbytes);
void sal_gpu_free(void *p);
int sal_gpu_copy_h2d(void *dst, const void *src, int64_t nbytes);
int sal_gpu_copy_d2h(void *dst, const void *src, int64_t nbytes);
int sal_gpu_copy_d2d(void *dst, const void *src, int64_t nbytes);
int sal_mem_place(const void *p);
void sal_gpu_sync(void);

void *sal_load_f32(const char *path, int64_t *out_elems);

/* Per-actor heaps: place 0=cpu, 1=gpu, 2=tpu. With SAL_USE_CUDA, gpu uses device memory. */
void *sal_place_malloc(int64_t size, int place);
void sal_place_free(void *p);
/* Copy `nbytes` onto `to_place` heap; returns dest pointer. */
void *sal_place_copy(const void *src, int64_t nbytes, int to_place);
/* Record a device-region launch on `place` (gpu/tpu heaps). */
void sal_on_enter(int64_t place);
int64_t sal_place_launches(int64_t place);

/* Process args (set from generated main via sal_runtime_init). */
void sal_runtime_init(int64_t argc, const char **argv);
int64_t sal_argc(void);
/* Heap copy of argv[i]; caller must sal_free. Empty string if OOB. */
char *sal_argv(int64_t i);

/* Read entire file into a heap buffer (NUL-terminated). Caller sal_free. */
char *sal_read_file(const char *path);
int64_t sal_path_readable(const char *path);
/* Write bytes to path (or stdout when path is NULL / "-"). */
int64_t sal_write_file(const char *path, const char *data);
/* Print NUL-terminated string to stdout (no extra newline). */
int64_t sal_print_str(const char *data);
/* Print NUL-terminated string to stderr (no extra newline). */
int64_t sal_eprint_str(const char *data);
/* Heap copy of getenv(name); empty string if unset. Caller sal_free. */
char *sal_getenv(const char *name);
/* mkdir -p style; returns 0 on success. */
int64_t sal_mkdir_p(const char *path);

int64_t sal_str_eq(const char *a, const char *b);
int64_t sal_str_contains(const char *hay, const char *needle);
int64_t sal_str_len(const char *s);
/* Heap copy of string bytes on cpu; returns data pointer as i64 (length = str_len(s)). */
int64_t sal_str_bytes(const char *s);
char *sal_str_concat(const char *a, const char *b);
/* Like concat but frees heap-owned `a` (b is borrowed). */
char *sal_str_append(char *a, const char *b);
char *sal_strdup(const char *s);
/* Both a and b must be heap-owned; frees the non-selected and returns the other. */
char *sal_select_str(int64_t cond, char *a, char *b);
void sal_free(void *p);
/* Copy file contents; returns 0 on success. */
int64_t sal_copy_file(const char *src, const char *dst);
/* Copy the running executable to dst (0755). */
int64_t sal_copy_self(const char *dst);
int64_t sal_not(int64_t x);
int64_t sal_gated_print_str(int64_t cond, const char *data);
int64_t sal_gated_copy_self(int64_t cond, const char *dst);

/* String / list helpers for the SAL selfhost compiler. */
int64_t sal_str_char(const char *s, int64_t i);
/* See sal_runtime.c for kind codes. limit <= 0 is unbounded except kinds 5 and 6. */
int64_t sal_str_skip(const char *s, int64_t i, int64_t kind, int64_t limit);
/* Wrapping djb-style hash: h = h * 33 + byte, from seed. */
int64_t sal_str_hash(const char *s, int64_t seed);
void *sal_map_new(void);
int64_t sal_map_get(void *m, const char *key);
int64_t sal_map_put(void *m, const char *key, int64_t val);
/* Text of a selfhost IR module (vec of functions). Same bytes as ir_to_text. */
char *sal_ir_text(void *mod);
void *sal_lex_src(const char *src);
char *sal_str_slice(const char *s, int64_t start, int64_t end);
char *sal_int_to_str(int64_t v);
char *sal_char_to_str(int64_t c);
char *sal_str_from_int(int64_t x);
int64_t sal_str_as_int(const char *s);
void *sal_vec_new(void);
int64_t sal_vec_push(void *v, int64_t x);
int64_t sal_vec_get(void *v, int64_t i);
int64_t sal_vec_set(void *v, int64_t i, int64_t x);
int64_t sal_vec_len(void *v);
void sal_vec_free(void *v);
/* White RGB buffer, w*h*3 bytes stored as i64. NULL-sized empty vec if out of range. */
void *sal_image_new(int64_t w, int64_t h);
/* Write an 8-bit RGB PNG. Returns 0 on success. */
int64_t sal_write_png(const char *path, int64_t w, int64_t h, void *rgb);

/* List[Int] surface: heap vector; push returns the same list for chaining. */
void *sal_list_new(void);
void *sal_list_push(void *v, int64_t x);
int64_t sal_list_len(void *v);
int64_t sal_list_get(void *v, int64_t i);
/* elem_kind: 0 Int, 1 Float (bits), 2 Bool, 3 String (ptr) */
void *sal_list_new_typed(int64_t elem_kind);
int64_t sal_list_get_typed(void *v, int64_t i);
void *sal_dict_new(int64_t key_kind, int64_t val_kind);
void *sal_dict_put(void *d, const char *key, int64_t val);
int64_t sal_dict_get(void *d, const char *key);
/* Compile a generated .c with clang, linking runtime+kernels+instrument (not selfhost). */
int64_t sal_clang(const char *c_path, const char *out_path);
/* Canonical path; falls back to a copy of p if realpath fails. */
char *sal_realpath(const char *p);
/* Compile .c to .o (no link). */
int64_t sal_clang_obj(const char *c_path, const char *obj_path);
/* Link space-separated .o paths with runtime; objs may be empty aside from objects. */
int64_t sal_link_objs(const char *objs, const char *out_path);
/* Heap path like /tmp/sal-<pid>-<suffix>; caller sal_free. */
char *sal_tmp_path(const char *suffix);
char *sal_exec_capture(const char *bin, const char *arg);
int64_t sal_exec_compile(const char *bin, const char *src, const char *outp);
int64_t sal_gated_exec_compile(int64_t cond, const char *bin, const char *src, const char *outp);

void sal_instrument_init(void);
void sal_instrument_shutdown(void);
void sal_instrument_alloc(void *p, int64_t size, int place, const char *site);
void sal_instrument_free(void *p);
void sal_instrument_check_f32(const float *data, int64_t n, const char *site);
void *sal_instrument_malloc(int64_t size, int place, const char *site);
void sal_instrument_check_ptr(const void *p, int64_t nbytes, int place, const char *site);
void sal_instrument_check_index(int64_t index, int64_t len, const char *site);

#endif

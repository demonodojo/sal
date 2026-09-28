/* TSan harness: shared rodata literal (cap==0) + per-thread heap strings. */
#include "../../runtime/sal_runtime.h"

#include <pthread.h>
#include <stdio.h>
#include <string.h>

static const struct __attribute__((aligned(16))) {
    int64_t cap;
    int64_t len;
    char b[6];
} g_shared_lit = {0, 5, "hello"};

static const char *shared_bytes = g_shared_lit.b;

static void *worker(void *arg) {
    (void)arg;
    char *a = sal_strdup(shared_bytes);
    char *b = sal_str_concat(a, shared_bytes);
    if (sal_str_len(b) != 10) {
        return (void *)1;
    }
    if (sal_str_len(b) != 10 || sal_str_char(b, 0) != (int64_t)'h') {
        sal_free(b);
        return (void *)1;
    }
    sal_free(b);
    return NULL;
}

int main(void) {
    enum { N = 8 };
    pthread_t th[N];
    for (int i = 0; i < N; i++) {
        if (pthread_create(&th[i], NULL, worker, NULL) != 0) {
            fprintf(stderr, "pthread_create failed\n");
            return 2;
        }
    }
    for (int i = 0; i < N; i++) {
        void *rc = NULL;
        pthread_join(th[i], &rc);
        if (rc != NULL) {
            return 1;
        }
    }
    return 0;
}

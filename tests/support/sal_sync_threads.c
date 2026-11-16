/* TSan harness for the thread primitives of SPEC «Hilos»: sal_thread_spawn/join,
 * Semaphore, Mutex[Int] (a cell) and Channel[Int], plus the tensor place
 * heaps and fused regions that several threads now share or keep per thread. */
#include "../../runtime/sal_runtime.h"

#include <stdio.h>

enum { N = 8, PER = 200 };

static int64_t g_sem;
static int64_t g_mutex;
static int64_t g_chan;
static int64_t g_inside; /* threads inside the semaphore; only read under the mutex */

/* Shape of a sal function: i64 (i64…). */
static int64_t worker(int64_t id) {
    for (int i = 0; i < PER; i++) {
        sal_sem_acquire(g_sem);
        int64_t cell = sal_mutex_enter(g_mutex);
        sal_cell_set(cell, sal_cell_get(cell) + 1);
        g_inside++;
        if (g_inside > 2) {
            sal_mutex_leave(g_mutex);
            return -1;
        }
        g_inside--;
        sal_mutex_leave(g_mutex);
        sal_sem_release(g_sem);

        void *t = sal_place_malloc(64, 0);
        sal_region_begin(0, 128);
        void *r = sal_place_malloc(32, 0);
        (void)r;
        sal_region_end(0);
        sal_place_free(t);
    }
    sal_chan_send(g_chan, id);
    return id * 10;
}

int main(void) {
    g_sem = sal_sem_new(2);
    g_mutex = sal_mutex_new_cell(0);
    g_chan = sal_chan_new((int64_t)(uintptr_t)sal_list_new_desc("i"));
    int64_t th[N];
    for (int i = 0; i < N; i++) {
        th[i] = sal_thread_spawn((int64_t)(uintptr_t)sal_list_new_desc("i"), (int64_t)(uintptr_t)worker, 1, i + 1,
                                 0, 0, 0, 0, 0, 0, 0);
    }
    int64_t sum = 0;
    for (int i = 0; i < N; i++) {
        int64_t r = sal_thread_join(th[i]);
        if (r < 0) {
            return 1;
        }
        sum += r;
    }
    sal_chan_close(g_chan);
    int64_t got = 0;
    for (;;) {
        int64_t opt = sal_chan_recv(g_chan);
        if (opt == 0) {
            break;
        }
        got++;
        sal_enum_free(opt);
    }
    int64_t total = sal_cell_get(sal_mutex_enter(g_mutex));
    sal_mutex_leave(g_mutex);
    sal_mutex_free(g_mutex);
    sal_chan_free(g_chan);
    sal_sem_free(g_sem);
    if (sum != 10 * N * (N + 1) / 2 || got != N || total != N * PER) {
        fprintf(stderr, "sum=%lld got=%lld total=%lld\n", (long long)sum, (long long)got,
                (long long)total);
        return 1;
    }
    return 0;
}

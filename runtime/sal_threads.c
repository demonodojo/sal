/* Threads and synchronisation (SPEC «Hilos»): Thread, Semaphore, Mutex[T], Channel[T] on
 * pthreads. A translation unit of its own next to sal_runtime.c; it only uses the runtime's
 * public functions (sal_runtime.h). */
#define _POSIX_C_SOURCE 200809L
#define _DEFAULT_SOURCE
#include "sal_runtime.h"

#ifndef SAL_SOSO
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* ---- Threads and synchronisation (SPEC «Hilos») ----------------------------------------
 * Every handle is an i64. A thread calls a sal function (`i64 @sal_u_f(i64…)`) with up to
 * eight i64 arguments; its result goes into a List[R] the compiler built with R's element
 * glue, so dropping an unjoined thread releases R like any list element. A Mutex[T] keeps
 * its value in a one-element List[T], a Channel[T] its queue in a List[T]. soso has no
 * threads in this cut (E_TARGET), so none of this is built there. */
enum { SAL_THREAD_MAX_ARGS = 8 };
/* Same stack as the main thread gets in sal_runtime_init: sal code recurses deeply. */
#define SAL_THREAD_STACK ((size_t)64 * 1024 * 1024)

typedef int64_t (*SalFn0)(void);
typedef int64_t (*SalFn1)(int64_t);
typedef int64_t (*SalFn2)(int64_t, int64_t);
typedef int64_t (*SalFn3)(int64_t, int64_t, int64_t);
typedef int64_t (*SalFn4)(int64_t, int64_t, int64_t, int64_t);
typedef int64_t (*SalFn5)(int64_t, int64_t, int64_t, int64_t, int64_t);
typedef int64_t (*SalFn6)(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t);
typedef int64_t (*SalFn7)(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t, int64_t);
typedef int64_t (*SalFn8)(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t, int64_t,
                          int64_t);

typedef struct {
    pthread_t tid;
    int64_t fn;
    int64_t argc;
    int64_t args[SAL_THREAD_MAX_ARGS];
    void *result;
} SalThread;

static int64_t sal_call_n(int64_t fn, int64_t argc, const int64_t *a) {
    void *f = (void *)(uintptr_t)fn;
    switch (argc) {
    case 0: return ((SalFn0)f)();
    case 1: return ((SalFn1)f)(a[0]);
    case 2: return ((SalFn2)f)(a[0], a[1]);
    case 3: return ((SalFn3)f)(a[0], a[1], a[2]);
    case 4: return ((SalFn4)f)(a[0], a[1], a[2], a[3]);
    case 5: return ((SalFn5)f)(a[0], a[1], a[2], a[3], a[4]);
    case 6: return ((SalFn6)f)(a[0], a[1], a[2], a[3], a[4], a[5]);
    case 7: return ((SalFn7)f)(a[0], a[1], a[2], a[3], a[4], a[5], a[6]);
    default: return ((SalFn8)f)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]);
    }
}

static void *sal_thread_main(void *vp) {
    SalThread *t = (SalThread *)vp;
    int64_t r = sal_call_n(t->fn, t->argc, t->args);
    sal_list_push(t->result, r);
    return NULL;
}

int64_t sal_thread_spawn(int64_t result, int64_t fn, int64_t argc, int64_t a0, int64_t a1,
                         int64_t a2, int64_t a3, int64_t a4, int64_t a5, int64_t a6,
                         int64_t a7) {
    if (argc < 0 || argc > SAL_THREAD_MAX_ARGS) {
        sal_panic("thread_spawn: too many arguments");
    }
    SalThread *t = (SalThread *)malloc(sizeof(SalThread));
    if (!t) {
        sal_panic("thread_spawn: out of memory");
    }
    t->fn = fn;
    t->argc = argc;
    int64_t a[SAL_THREAD_MAX_ARGS] = {a0, a1, a2, a3, a4, a5, a6, a7};
    memcpy(t->args, a, sizeof(a));
    t->result = (void *)(uintptr_t)result;
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, SAL_THREAD_STACK);
    int rc = pthread_create(&t->tid, &attr, sal_thread_main, t);
    pthread_attr_destroy(&attr);
    if (rc != 0) {
        sal_panic("thread_spawn: cannot create thread");
    }
    return (int64_t)(uintptr_t)t;
}

/* `t.join()`: waits, moves R out to the caller and releases the handle. */
int64_t sal_thread_join(int64_t h) {
    SalThread *t = (SalThread *)(uintptr_t)h;
    pthread_join(t->tid, NULL);
    int64_t r = sal_list_pop(t->result);
    sal_list_free(t->result);
    free(t);
    return r;
}

/* Drop glue of a Thread nobody joined: join, then release R with its glue. */
void sal_thread_free(int64_t h) {
    if (h == 0) {
        return;
    }
    SalThread *t = (SalThread *)(uintptr_t)h;
    pthread_join(t->tid, NULL);
    sal_list_free(t->result);
    free(t);
}

typedef struct {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int64_t permits;
} SalSemaphore;

int64_t sal_sem_new(int64_t n) {
    if (n < 0) {
        sal_panic("Semaphore.new: negative permits");
    }
    SalSemaphore *s = (SalSemaphore *)malloc(sizeof(SalSemaphore));
    if (!s) {
        sal_panic("Semaphore.new: out of memory");
    }
    pthread_mutex_init(&s->mu, NULL);
    pthread_cond_init(&s->cv, NULL);
    s->permits = n;
    return (int64_t)(uintptr_t)s;
}

int64_t sal_sem_acquire(int64_t h) {
    SalSemaphore *s = (SalSemaphore *)(uintptr_t)h;
    pthread_mutex_lock(&s->mu);
    while (s->permits == 0) {
        pthread_cond_wait(&s->cv, &s->mu);
    }
    s->permits--;
    pthread_mutex_unlock(&s->mu);
    return 0;
}

int64_t sal_sem_try_acquire(int64_t h) {
    SalSemaphore *s = (SalSemaphore *)(uintptr_t)h;
    pthread_mutex_lock(&s->mu);
    int64_t ok = s->permits > 0;
    if (ok) {
        s->permits--;
    }
    pthread_mutex_unlock(&s->mu);
    return ok;
}

int64_t sal_sem_release(int64_t h) {
    SalSemaphore *s = (SalSemaphore *)(uintptr_t)h;
    pthread_mutex_lock(&s->mu);
    if (s->permits == INT64_MAX) {
        pthread_mutex_unlock(&s->mu);
        sal_panic("Semaphore.release: too many permits");
    }
    s->permits++;
    pthread_cond_signal(&s->cv);
    pthread_mutex_unlock(&s->mu);
    return 0;
}

void sal_sem_free(int64_t h) {
    if (h == 0) {
        return;
    }
    SalSemaphore *s = (SalSemaphore *)(uintptr_t)h;
    pthread_cond_destroy(&s->cv);
    pthread_mutex_destroy(&s->mu);
    free(s);
}

/* A Mutex[T] keeps its value where a `&mut T` can point at it: a T without a block of its
 * own (Int, Float, Bool, a one-field struct of those) lives in a cell (`sal_cell_*`), which
 * is what a `&mut T` parameter receives; any other T is a handle kept in a one-element
 * List[T], whose glue releases it with the mutex. A `String` (or a one-field struct around
 * one) also goes in that list, and `enter` hands out its slot, which is a cell. */
typedef struct {
    pthread_mutex_t mu;
    void *list;
    int64_t cell;
    int slot;
} SalMutex;

static SalMutex *sal_mutex_alloc(void) {
    SalMutex *m = (SalMutex *)malloc(sizeof(SalMutex));
    if (!m) {
        sal_panic("Mutex.new: out of memory");
    }
    pthread_mutex_init(&m->mu, NULL);
    m->list = NULL;
    m->cell = 0;
    m->slot = 0;
    return m;
}

/* `list` holds the value (a heap T). */
int64_t sal_mutex_new(int64_t list) {
    SalMutex *m = sal_mutex_alloc();
    m->list = (void *)(uintptr_t)list;
    return (int64_t)(uintptr_t)m;
}

/* `list` holds a T that a `&mut T` reaches through a cell and that has to be released. */
int64_t sal_mutex_new_slot(int64_t list) {
    SalMutex *m = sal_mutex_alloc();
    m->list = (void *)(uintptr_t)list;
    m->slot = 1;
    return (int64_t)(uintptr_t)m;
}

/* `v` is a T without a block of its own. */
int64_t sal_mutex_new_cell(int64_t v) {
    SalMutex *m = sal_mutex_alloc();
    m->cell = sal_cell_new(v);
    return (int64_t)(uintptr_t)m;
}

/* Locks and hands out the `&mut T`: the cell, or the handle of the value. */
int64_t sal_mutex_enter(int64_t h) {
    SalMutex *m = (SalMutex *)(uintptr_t)h;
    pthread_mutex_lock(&m->mu);
    if (m->cell) {
        return m->cell;
    }
    return m->slot ? sal_list_slot(m->list, 0) : sal_list_get(m->list, 0);
}

int64_t sal_mutex_leave(int64_t h) {
    SalMutex *m = (SalMutex *)(uintptr_t)h;
    pthread_mutex_unlock(&m->mu);
    return 0;
}

void sal_mutex_free(int64_t h) {
    if (h == 0) {
        return;
    }
    SalMutex *m = (SalMutex *)(uintptr_t)h;
    if (m->list) {
        sal_list_free(m->list);
    }
    if (m->cell) {
        sal_cell_free(m->cell);
    }
    pthread_mutex_destroy(&m->mu);
    free(m);
}

typedef struct {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    void *queue; /* List[T]: pending values, oldest first */
    int closed;
} SalChannel;

int64_t sal_chan_new(int64_t queue) {
    SalChannel *c = (SalChannel *)malloc(sizeof(SalChannel));
    if (!c) {
        sal_panic("Channel.new: out of memory");
    }
    pthread_mutex_init(&c->mu, NULL);
    pthread_cond_init(&c->cv, NULL);
    c->queue = (void *)(uintptr_t)queue;
    c->closed = 0;
    return (int64_t)(uintptr_t)c;
}

/* Moves `v` into the queue. Sending on a closed channel is a panic. */
int64_t sal_chan_send(int64_t h, int64_t v) {
    SalChannel *c = (SalChannel *)(uintptr_t)h;
    pthread_mutex_lock(&c->mu);
    if (c->closed) {
        pthread_mutex_unlock(&c->mu);
        sal_panic("Channel.send: channel is closed");
    }
    sal_list_push(c->queue, v);
    pthread_cond_signal(&c->cv);
    pthread_mutex_unlock(&c->mu);
    return 0;
}

/* `Option[T]`: waits for a value; None (0) once the channel is closed and empty. */
int64_t sal_chan_recv(int64_t h) {
    SalChannel *c = (SalChannel *)(uintptr_t)h;
    pthread_mutex_lock(&c->mu);
    void *q = c->queue;
    while (sal_list_len(q) == 0 && !c->closed) {
        pthread_cond_wait(&c->cv, &c->mu);
    }
    if (sal_list_len(q) == 0) {
        pthread_mutex_unlock(&c->mu);
        return 0;
    }
    int64_t raw = sal_list_take_front(q);
    int is_str = (int)sal_list_holds_strings(q);
    pthread_mutex_unlock(&c->mu);
    /* Same Some box as sal_dict_get. */
    int64_t some = sal_enum_new(2, 1);
    if (is_str) {
        int64_t *box = (int64_t *)(intptr_t)some;
        box[0] = 2 | ((int64_t)1 << 8);
    }
    sal_enum_set(some, 0, raw);
    return some;
}

int64_t sal_chan_close(int64_t h) {
    SalChannel *c = (SalChannel *)(uintptr_t)h;
    pthread_mutex_lock(&c->mu);
    c->closed = 1;
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
    return 0;
}

void sal_chan_free(int64_t h) {
    if (h == 0) {
        return;
    }
    SalChannel *c = (SalChannel *)(uintptr_t)h;
    sal_list_free(c->queue);
    pthread_cond_destroy(&c->cv);
    pthread_mutex_destroy(&c->mu);
    free(c);
}
#endif

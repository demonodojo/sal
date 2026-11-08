#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
void *sal_list_new_desc(const char *desc);
void sal_list_set_drop(void *vp, void (*glue)(int64_t));
void sal_list_free(void *vp);
void *sal_list_push(void *vp, int64_t x);
static int calls = 0;
static int64_t seen[8];
static void glue(int64_t x) { if (calls < 8) seen[calls] = x; calls++; }
int main(void) {
    void *l = sal_list_new_desc("i");
    sal_list_set_drop(l, glue);
    l = sal_list_push(l, 11);
    l = sal_list_push(l, 22);
    sal_list_free(l);
    printf("calls=%d first=%lld second=%lld\n", calls, (long long)seen[0], (long long)seen[1]);
    return calls == 2 && seen[0] == 11 && seen[1] == 22 ? 0 : 1;
}

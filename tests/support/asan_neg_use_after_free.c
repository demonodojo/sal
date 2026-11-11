#include <stdint.h>
void *sal_list_new_desc(const char *desc);
void *sal_list_push(void *vp, int64_t x);
void sal_list_free(void *vp);
/* Deliberate use after free: push into a freed list. AddressSanitizer must report it. */
int main(void) {
    void *l = sal_list_new_desc("i");
    l = sal_list_push(l, 1);
    sal_list_free(l);
    sal_list_push(l, 2);
    return 0;
}

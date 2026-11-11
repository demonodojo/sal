#include <stdint.h>
void *sal_list_new_desc(const char *desc);
void sal_list_free(void *vp);
/* Deliberate double free. AddressSanitizer must report it. */
int main(void) {
    void *l = sal_list_new_desc("i");
    sal_list_free(l);
    sal_list_free(l);
    return 0;
}

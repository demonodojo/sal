#include "../../runtime/sal_runtime.h"

int main(void) {
    sal_instrument_init();
    void *p = sal_instrument_malloc(16, 0, "uaf_site");
    sal_instrument_free(p);
    sal_instrument_check_ptr(p, 1, 0, "uaf_site");
    sal_instrument_shutdown();
    return 0;
}

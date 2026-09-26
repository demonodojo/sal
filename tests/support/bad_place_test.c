#include "../../runtime/sal_runtime.h"

int main(void) {
    sal_instrument_init();
    /* Allocated on cpu (place 0), accessed as gpu (place 1). */
    void *p = sal_instrument_malloc(16, 0, "bad_place_site");
    sal_instrument_check_ptr(p, 1, 1, "bad_place_site");
    sal_instrument_shutdown();
    return 0;
}

#include "../../runtime/sal_runtime.h"

int main(void) {
    sal_instrument_init();
    void *p = sal_instrument_malloc(8, 0, "oob_redzone");
    /* Access one byte past the user region (into the red zone). */
    sal_instrument_check_ptr((char *)p + 8, 1, 0, "oob_redzone");
    sal_instrument_shutdown();
    return 0;
}

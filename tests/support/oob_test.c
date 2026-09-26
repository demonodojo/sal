#include "../../runtime/sal_runtime.h"

int main(void) {
    sal_instrument_init();
    sal_instrument_check_index(5, 3, "oob_index");
    sal_instrument_shutdown();
    return 0;
}

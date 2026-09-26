#include "../../runtime/sal_runtime.h"

int main(void) {
    sal_instrument_init();
    (void)sal_instrument_malloc(64, 0, "leak_site");
    sal_instrument_shutdown();
    return 0;
}

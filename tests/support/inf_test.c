#include "../../runtime/sal_runtime.h"
#include <math.h>

int main(void) {
    sal_instrument_init();
    float x = INFINITY;
    sal_instrument_check_f32(&x, 1, "inf_site");
    sal_instrument_shutdown();
    return 0;
}

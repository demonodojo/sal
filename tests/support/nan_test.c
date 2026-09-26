#include "../../runtime/sal_runtime.h"
#include <math.h>

int main(void) {
    sal_instrument_init();
    float x = NAN;
    sal_instrument_check_f32(&x, 1, "test");
    sal_instrument_shutdown();
    return 0;
}

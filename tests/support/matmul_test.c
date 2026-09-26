#include "../../runtime/sal_runtime.h"
#include <assert.h>
#include <stdio.h>

int main(void) {
    float a[] = {1, 2, 3, 4};
    float b[] = {1, 0, 0, 1};
    float out[4];
    sal_matmul_f32(a, b, out, 2, 2, 2);
    assert(out[0] == 1.f);
    assert(out[1] == 2.f);
    assert(out[2] == 3.f);
    assert(out[3] == 4.f);
    return 0;
}

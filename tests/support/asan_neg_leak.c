#include <stdlib.h>
#include <string.h>
/* Deliberate leak. The pointer is overwritten before exit, so LeakSanitizer must report the block.
 * (Leaking through the runtime API was not reliable: stale copies of the pointer in registers or
 * stack slots keep the block "reachable".) This proves leak detection is on for this harness. */
int main(void) {
    char *volatile p = malloc(64);
    memset(p, 1, 64);
    p = NULL;
    return 0;
}

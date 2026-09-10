/* Linux userspace harness for comparing the codec with captured hardware. */
#include "ave2-wire.h"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int number(const char *s, uint64_t *out)
{
    char *end;
    if (!s[0] || s[0] == '-') return -1;
    errno = 0;
    *out = strtoull(s, &end, 0);
    return errno || *end ? -1 : 0;
}

int main(int argc, char **argv)
{
    uint64_t a[5];
    uint8_t slot[64];
    if (argc != 7 || strlen(argv[6]) != 128) return 2;
    for (int i = 0; i < 5; ++i)
        if (number(argv[i+1], &a[i])) return 2;
    if (a[0] > UINT32_MAX || a[1] > UINT32_MAX || a[3] > UINT32_MAX || a[4] > UINT32_MAX)
        return 2;
    for (unsigned i = 0; i < 64; ++i) {
        unsigned value;
        char pair[3] = {argv[6][i*2], argv[6][i*2+1], 0};
        if (strspn(pair, "0123456789abcdefABCDEF") != 2 || sscanf(pair, "%2x", &value) != 1)
            return 2;
        slot[i] = (uint8_t)value;
    }
    if (ave2_slot_encode(slot, (unsigned)a[0], (unsigned)a[1], a[2], (uint32_t)a[3], (uint32_t)a[4]))
        return 3;
    struct ave2_message decoded;
    if (ave2_slot_decode(slot, (unsigned)a[0], &decoded) || decoded.address != a[2] ||
        decoded.length != a[3] || decoded.flags != a[4] || decoded.control_bits != (a[1] ^ 1))
        return 4;
    for (unsigned i = 0; i < 64; ++i) printf("%02x", slot[i]);
    puts("");
    return 0;
}

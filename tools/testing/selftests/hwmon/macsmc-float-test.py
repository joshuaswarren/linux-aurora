#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Exercise the actual SMC float reader with a fake transport, under UBSan."""
import os
from pathlib import Path
import random
import subprocess
import tempfile

root = Path(__file__).resolve().parents[4]
source = (root / "drivers/hwmon/macsmc-hwmon.c").read_text()
start = source.index("static int macsmc_hwmon_read_f32_scaled(")
end = source.index("\n/*", start)
reader = source[start:end]


def expected(bits, scale):
    exponent = (bits >> 23) & 255
    if exponent == 255:
        return "-ENODATA", 0
    mantissa = bits & 0x7fffff
    if exponent:
        mantissa |= 1 << 23
    shift = (exponent or 1) - 127 - 23
    magnitude = mantissa * scale
    magnitude = magnitude << shift if shift >= 0 else magnitude >> -shift
    value = -magnitude if bits >> 31 else magnitude
    if not -(1 << 63) <= value < (1 << 63):
        return "-ERANGE", 0
    return "0", value


rng = random.Random(8140)
bits = [0, 0x80000000, 1, 0x80000001, 0x7fffff, 0x800000,
        0x42200000, 0xc1200000, 0x7f800000, 0xff800000,
        0x7fc00000, 0x7f800001, 0x7f7fffff, 0xff7fffff,
        0xdf000000]  # -2**63: negating LONG_MIN must not overflow.
bits += [rng.getrandbits(32) for _ in range(3000)]
cases = []
for raw in bits:
    for scale in (1, 1000, 1000000):
        error, value = expected(raw, scale)
        literal = "LONG_MIN" if value == -(1 << 63) else f"{value or 0}L"
        cases.append(f"{{0x{raw:08x}, {scale}, {error}, {literal}}}")

program = r"""
#include <stdint.h>
#include <stdio.h>
#include <limits.h>
#include <errno.h>
typedef uint32_t u32;
typedef uint64_t u64;
typedef u32 smc_key;
#define U64_MAX UINT64_MAX
#define BIT(n) (1ULL << (n))
#define FLT_EXP_MASK 0x7f800000U
#define FLT_MANT_MASK 0x007fffffU
#define FLT_SIGN_MASK 0x80000000U
#define FLT_EXP_BIAS 127
#define FLT_MANT_BIAS 23
#define FIELD_GET(mask, val) (((val) & (mask)) >> __builtin_ctz(mask))
struct apple_smc { u32 bits; int error; };
static int apple_smc_read_u32(struct apple_smc *smc, smc_key key, u32 *out)
{
    (void)key;
    if (smc->error)
        return smc->error;
    *out = smc->bits;
    return 0;
}
""" + reader + "\nstatic const struct { u32 bits; int scale, error; long value; } cases[] = {\n"
program += ",\n".join(cases) + r"""
};
int main(void)
{
    _Static_assert(sizeof(long) == 8, "test oracle requires a 64-bit host");
    struct apple_smc smc = {0};
    long out;
    int ret;
    for (unsigned int i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        smc.bits = cases[i].bits;
        out = 123;
        ret = macsmc_hwmon_read_f32_scaled(&smc, 0, &out, cases[i].scale);
        if (ret != cases[i].error ||
            out != (ret ? 123 : cases[i].value)) {
            fprintf(stderr, "case %u bits=%08x scale=%d ret=%d value=%ld\n",
                    i, smc.bits, cases[i].scale, ret, out);
            return 1;
        }
    }
    smc.error = -ETIMEDOUT;
    out = 123;
    if (macsmc_hwmon_read_f32_scaled(&smc, 0, &out, 1000) != -ETIMEDOUT || out != 123)
        return 1;
    puts("PASS: SMC float conversion, nonfinite/unavailable and transport errors");
    return 0;
}
"""
with tempfile.TemporaryDirectory(prefix="macsmc-float-test-") as temporary:
    directory = Path(temporary)
    cfile = directory / "test.c"
    binary = directory / "test"
    cfile.write_text(program)
    subprocess.run([os.environ.get("HOSTCC", "cc"), "-std=gnu11", "-O1", "-g",
                    "-Wall", "-Wextra", "-Werror", "-fsanitize=undefined",
                    "-fno-sanitize-recover=all", str(cfile), "-o", str(binary)],
                   check=True)
    subprocess.run([str(binary)], check=True)
print(f"PASS: {len(cases)} exact-integer oracle cases")

#!/usr/bin/env python3
"""Exercise the actual Linux AFK initializer with bounded firmware fixtures."""
from pathlib import Path
import subprocess
import tempfile

root = next(candidate for parent in Path(__file__).resolve().parents
            for candidate in (parent, parent / 'linux')
            if (candidate / 'drivers/gpu/drm/apple/afk.c').is_file())
source = (root / "drivers/gpu/drm/apple/afk.c").read_text()
header = (root / "drivers/gpu/drm/apple/afk.h").read_text()
initializer = source.split("static void afk_init_rxtx", 1)[1].split(
    "\n#if IS_ENABLED", 1
)[0]
ring = header.split("struct afk_ringbuffer {", 1)[1].split("\n};", 1)[0]
accessors = header.split("static inline __le32 *afk_rb_field", 1)[1].split(
    "\nstruct apple_dcp_afkep", 1
)[0]
prelude = r"""
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
typedef uint16_t u16;
typedef uint8_t u8;
typedef uint32_t u32;
typedef uint64_t u64;
typedef uint32_t __le32;
#define BLOCK_SHIFT 6
#define AFK_RB_BLOCK_MIN 0x40
#define AFK_RB_BLOCKS 3
#define BIT(n) (1U << (n))
#define INITRB_OFFSET UINT64_C(0xffff00000000)
#define INITRB_SIZE UINT64_C(0xffff0000)
#define INITRB_TAG UINT64_C(0xffff)
#define RBEP_TYPE UINT64_C(0xffff000000000000)
#define RBEP_START 0xa3
#define FIELD_GET(mask, val) (((val) & (mask)) >> __builtin_ctzll(mask))
#define FIELD_PREP(mask, val) (((u64)(val) << __builtin_ctzll(mask)) & (mask))
#define READ_ONCE(x) (x)
#define le32_to_cpu(x) (x)
#define IS_ALIGNED(x, a) (((x) & ((a) - 1)) == 0)
#define is_power_of_2(x) ((x) && !((x) & ((x) - 1)))
#define dma_rmb() ((void)0)
#define dev_err(...) ((void)0)
#define dev_dbg(...) ((void)0)
"""
ep = r"""
struct apple_dcp_afkep {
    void *bfr;
    size_t bfr_size;
    u16 bfr_tag;
    struct afk_ringbuffer rxbfr, txbfr;
};
static unsigned int starts;
#define afk_send(ep, message) (++starts)
"""
tests = r"""
static unsigned char memory[0x30000] __attribute__((aligned(128)));
static u64 message(u32 base, u32 size, u16 tag)
{
    return FIELD_PREP(INITRB_OFFSET, base >> 6) |
           FIELD_PREP(INITRB_SIZE, size >> 6) | tag;
}
static struct apple_dcp_afkep setup(u32 base, u32 data_size)
{
    memset(memory, 0xa5, sizeof(memory));
    memcpy(memory + base, &data_size, sizeof(data_size));
    return (struct apple_dcp_afkep) {
        .bfr = memory, .bfr_size = sizeof(memory), .bfr_tag = 7,
    };
}
int main(void)
{
    const u32 accepted[][4] = {
        {0, 0x4000, 0x3f40, 0x40},
        {0, 0x4000, 0x3e80, 0x80},
        {0x4000, 0x2000, 0x1e80, 0x80},
        {0x10000, 0x10000, 0xfe80, 0x80},
    };
    for (unsigned int i = 0; i < sizeof(accepted)/sizeof(*accepted); ++i) {
        u32 base = accepted[i][0], size = accepted[i][1];
        u32 data = accepted[i][2], block = accepted[i][3];
        struct apple_dcp_afkep ep = setup(base, data);
        starts = 0;
        afk_init_rxtx(&ep, message(base, size, 7), &ep.rxbfr);
        assert(ep.rxbfr.ready && !starts);
        assert(afk_rb_rptr(&ep.rxbfr) == (__le32 *)(memory + base + block));
        assert(afk_rb_wptr(&ep.rxbfr) == (__le32 *)(memory + base + 2 * block));
        assert(ep.rxbfr.buf == memory + base + 3 * block);
        assert(ep.rxbfr.bufsz == data && ep.rxbfr.block == block);
        assert(*afk_rb_rptr(&ep.rxbfr) == 0xa5a5a5a5); /* preserve firmware indices */
        afk_init_rxtx(&ep, message(base, size, 7), &ep.txbfr);
        assert(ep.txbfr.ready && starts == 1);
        afk_init_rxtx(&ep, message(base, size, 7), &ep.txbfr);
        assert(starts == 1);
    }
    const u32 rejected[][3] = {
        {0, 0x4000, 0x4000}, {0, 0x4000, 0x4001},
        {0, 0x4000, 0x3e81}, {0, 0x4000, 0x3fa0},
        {0, 0x4000, 0x3dc0}, {0, 0x4000, 0},
        {0, 0, 0}, {0, 0x100, 0x40},
        {0x28000, 0x10000, 0xfe80},
    };
    for (unsigned int i = 0; i < sizeof(rejected)/sizeof(*rejected); ++i) {
        struct apple_dcp_afkep ep = setup(rejected[i][0], rejected[i][2]);
        afk_init_rxtx(&ep, message(rejected[i][0], rejected[i][1], 7), &ep.rxbfr);
        assert(!ep.rxbfr.ready && !ep.rxbfr.block && !ep.rxbfr.buf);
    }
    struct apple_dcp_afkep ep = setup(0, 0x3e80);
    afk_init_rxtx(&ep, message(0, 0x4000, 8), &ep.rxbfr);
    assert(!ep.rxbfr.ready);
    ep.bfr = NULL;
    afk_init_rxtx(&ep, message(0, 0x4000, 7), &ep.rxbfr);
    assert(!ep.rxbfr.ready);
    puts("AFK actual initializer: 4 valid layouts, 11 malformed inputs passed");
}
"""
with tempfile.TemporaryDirectory(prefix="m5-afk-layout-") as directory:
    test = Path(directory) / "test.c"
    binary = Path(directory) / "test"
    test.write_text(prelude + "\nstruct afk_ringbuffer {" + ring + "\n};\n" +
                    "static inline __le32 *afk_rb_field" + accessors +
                    ep + "\nstatic void afk_init_rxtx" + initializer + tests)
    subprocess.run(["clang", "-std=gnu11", "-Wall", "-Werror", "-O1",
                    "-fsanitize=address,undefined", str(test), "-o", str(binary)],
                   check=True)
    subprocess.run([str(binary)], check=True)

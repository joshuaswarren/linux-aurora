#!/usr/bin/env python3
"""Compile the actual T8130 AUX helpers against native-sequence MMIO checks."""
from pathlib import Path
import re
import subprocess
import tempfile


def function(source, name):
    start = source.index("static void " + name + "(")
    body = source.index("{", start)
    depth = 1
    end = body + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


def main():
    root = Path(__file__).resolve().parents[5]
    source = (root / "drivers/phy/apple/atc.c").read_text()
    definitions = "\n".join(re.findall(r"^#define T8130_AUX_.*$", source, re.M))
    helpers = "\n".join(function(source, name) for name in (
        "atcphy_t8130_enable_dp_aux", "atcphy_t8130_disable_dp_aux",
        "atcphy_t8130_apply_dp_tunables", "atcphy_apply_tunables"))
    harness = r'''
#include <assert.h>
#include <stdint.h>
#include <stdbool.h>
#define BIT(n) (1U << (n))
#define dev_info_once(...) ((void)0)
enum atcphy_mode {
    APPLE_ATCPHY_MODE_OFF, APPLE_ATCPHY_MODE_USB2, APPLE_ATCPHY_MODE_USB3,
    APPLE_ATCPHY_MODE_USB3_DP, APPLE_ATCPHY_MODE_TBT, APPLE_ATCPHY_MODE_USB4,
    APPLE_ATCPHY_MODE_DP,
};
struct atcphy_hw { bool has_t8130_aux; };
struct apple_atcphy {
    uint32_t top, shm;
    const struct atcphy_hw *hw;
    bool fixed_usb2, swap_lanes;
    struct { unsigned core, axi2af, lpdptx; } regs;
    struct {
        unsigned common[2], axi2af, lane_usb3[2], lane_dp[2], lane_usb4[2];
        unsigned dp_common[2], dp_lpdptx;
    } tunables;
};
struct event { char kind; unsigned offset, value; };
static struct event events[24];
static unsigned count;
static void record(char kind, unsigned offset, unsigned value)
{
    assert(count < 24);
    events[count++] = (struct event){kind, offset, value};
}
static void core_mask32(struct apple_atcphy *phy, unsigned offset,
                        uint32_t mask, uint32_t value)
{
    assert(offset == 0x16000 || offset == 0x16400);
    uint32_t *reg = offset == 0x16000 ? &phy->top : &phy->shm;
    record('r', offset, *reg);
    *reg = (*reg & ~mask) | value;
    record('w', offset, *reg);
}
static void core_set32(struct apple_atcphy *phy, unsigned offset, uint32_t bits)
{ core_mask32(phy, offset, 0, bits); }
static void core_clear32(struct apple_atcphy *phy, unsigned offset, uint32_t bits)
{ core_mask32(phy, offset, bits, 0); }
static void udelay(unsigned usec) { record('d', 0, usec); }
static void apple_tunable_apply(unsigned window, unsigned table)
{ if (table) record('t', window, table); }
'''
    checks = r'''
static void check(const struct event *expected)
{
    assert(count == 12);
    for (unsigned i = 0; i < count; ++i) {
        assert(events[i].kind == expected[i].kind);
        assert(events[i].offset == expected[i].offset);
        assert(events[i].value == expected[i].value);
    }
    count = 0;
}
int main(void)
{
    struct apple_atcphy phy = {.top = 0xa5000001, .shm = 0x5a000200};
    /* 25G83 aciophy_aux_init: four read/write/delay steps. */
    const struct event enable[] = {
        {'r',0x16400,0x5a000200}, {'w',0x16400,0x5a000203}, {'d',0,1},
        {'r',0x16400,0x5a000203}, {'w',0x16400,0x5a00020f}, {'d',0,1},
        {'r',0x16400,0x5a00020f}, {'w',0x16400,0x5a00010f}, {'d',0,1},
        {'r',0x16000,0xa5000001}, {'w',0x16000,0xa5000000}, {'d',0,1},
    };
    atcphy_t8130_enable_dp_aux(&phy);
    check(enable);
    /* Shutdown keeps unrelated bits and the native override ordering. */
    const struct event disable[] = {
        {'r',0x16000,0xa5000000}, {'w',0x16000,0xa5000001}, {'d',0,1},
        {'r',0x16400,0x5a00010f}, {'w',0x16400,0x5a00030f}, {'d',0,1},
        {'r',0x16400,0x5a00030f}, {'w',0x16400,0x5a000307}, {'d',0,1},
        {'r',0x16400,0x5a000307}, {'w',0x16400,0x5a000305}, {'d',0,1},
    };
    atcphy_t8130_disable_dp_aux(&phy);
    check(disable);
    const struct atcphy_hw t8130 = {.has_t8130_aux = true};
    const struct atcphy_hw legacy = {0};
    phy.hw = &t8130;
    phy.regs.core = 1;
    phy.regs.axi2af = 2;
    phy.regs.lpdptx = 3;
    phy.tunables.common[0] = 10;
    phy.tunables.common[1] = 11;
    phy.tunables.axi2af = 12;
    phy.tunables.lane_usb3[0] = 13;
    phy.tunables.lane_usb3[1] = 14;
    phy.tunables.dp_common[0] = 15;
    phy.tunables.dp_common[1] = 16;
    phy.tunables.dp_lpdptx = 17;
    const struct event dp[] = {
        {'t',2,12}, {'t',1,15}, {'t',3,17}, {'t',1,16},
    };
    const struct event combined[] = {
        {'t',1,10}, {'t',2,12}, {'t',1,11}, {'t',1,13},
        {'t',2,12}, {'t',1,15}, {'t',3,17}, {'t',1,16},
    };
    atcphy_apply_tunables(&phy, APPLE_ATCPHY_MODE_DP);
    assert(count == 4);
    for (unsigned i = 0; i < count; ++i) {
        assert(events[i].kind == dp[i].kind);
        assert(events[i].offset == dp[i].offset);
        assert(events[i].value == dp[i].value);
    }
    count = 0;
    atcphy_apply_tunables(&phy, APPLE_ATCPHY_MODE_USB3_DP);
    assert(count == 8);
    for (unsigned i = 0; i < count; ++i) {
        assert(events[i].kind == combined[i].kind);
        assert(events[i].offset == combined[i].offset);
        assert(events[i].value == combined[i].value);
    }
    count = 0;
    phy.swap_lanes = true;
    atcphy_apply_tunables(&phy, APPLE_ATCPHY_MODE_USB3_DP);
    assert(count == 8 && events[3].value == 14);
    count = 0;
    phy.hw = &legacy;
    phy.tunables.lane_dp[0] = 18;
    phy.tunables.lane_dp[1] = 19;
    atcphy_apply_tunables(&phy, APPLE_ATCPHY_MODE_DP);
    assert(count == 5);
    assert(events[0].value == 10 && events[1].value == 12 && events[2].value == 11);
    assert(events[3].value == 19 && events[4].value == 18);
    count = 0;
    phy.hw = &t8130;
    phy.fixed_usb2 = true;
    atcphy_apply_tunables(&phy, APPLE_ATCPHY_MODE_USB2);
    assert(count == 0);
    return 0;
}
'''
    with tempfile.TemporaryDirectory() as temporary:
        c = Path(temporary) / "aux.c"
        exe = Path(temporary) / "aux"
        c.write_text(harness + definitions + "\n" + helpers + checks)
        subprocess.run(["clang", "-Wall", "-Wextra", "-Werror",
                        "-fsanitize=address,undefined", str(c), "-o", str(exe)], check=True)
        subprocess.run([str(exe)], check=True)
    print("T8130 native AUX MMIO and DP tunable ordering: PASS")


if __name__ == "__main__":
    main()

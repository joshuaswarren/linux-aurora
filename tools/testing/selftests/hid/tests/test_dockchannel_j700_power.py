# SPDX-License-Identifier: GPL-2.0
"""Exercise J700 DockChannel transactional power messages in userspace."""

from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[5]


class J700PowerTest(unittest.TestCase):
    def test_transactional_messages_and_legacy_fallback(self):
        compiler = shutil.which("clang") or shutil.which("cc")
        if compiler is None:
            self.skipTest("C compiler required")

        source = (ROOT / "drivers/hid/dockchannel-hid/dockchannel-hid.c").read_text()
        constants = "\n".join(
            line for line in source.splitlines()
            if re.match(
                r"#define (CMD_SET_POWER|DCHID_POWER_|DCHID_AFE_|DCHID_IPD_)\w*\s",
                line,
            )
        )
        start = source.index("static bool dchid_uses_transactional_power(")
        end = source.index("\nstatic int dchid_send_firmware", start)
        functions = source[start:end]

        harness = r'''
#include <assert.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
typedef uint8_t u8;
typedef uint32_t u32;
typedef long long ktime_t;
struct device_node { bool enabled; const char *firmware; };
struct device { struct device_node *of_node; };
struct dockchannel_hid { struct device *dev; };
struct gpio_desc;
struct apple_smc;
struct dchid_iface {
    struct dockchannel_hid *dchid;
    int index;
    const char *name;
    const struct device_node *of_node;
    struct gpio_desc *afe_reset;
    struct apple_smc *smc;
};
#define ENODEV 19
#define SMC_KEY_IPD_PWR_SEQ 0x704d4950
/* Consume every argument so the driver's log-only locals stay "used". */
static void dchid_test_log(const void *dev, const char *fmt, ...) {
    va_list ap;
    (void)dev;
    va_start(ap, fmt);
    va_end(ap);
}
#define dev_err(dev, ...) dchid_test_log(dev, __VA_ARGS__)
#define dev_info(dev, ...) dchid_test_log(dev, __VA_ARGS__)
#define dev_warn(dev, ...) dchid_test_log(dev, __VA_ARGS__)
static bool ipd_reset_sequence = true;
static bool ipd_reset_write = true;
static int power_method = -1;
/* Defaults asserted against the personality constants in main(). */
static int afe_enable_level = 0;
static int afe_disable_level = 1;
static unsigned char sent[4][16];
static size_t sent_size[4];
static int sent_count, fail_at = -1, slept_us;
/*
 * Interleaved trace: 'w' will-change, 'h' has-changed, '0'/'1' raw AFE supply
 * level, 'p'/'P' pmIP written 0/1.
 */
static char trace[16];
static int trace_len;
static int gpio_fail, smc_present = 1, smc_fail;
static u8 smc_value = 1;
static int afe_level = 1;
static bool of_property_read_bool(const struct device_node *node,
                                  const char *property) {
    (void)property;
    return node && node->enabled;
}
static int of_property_read_string(const struct device_node *node,
                                   const char *property, const char **value) {
    (void)property;
    if (!node || !node->firmware) return -1;
    *value = node->firmware;
    return 0;
}
static int dchid_comm_cmd(struct dockchannel_hid *dchid, void *msg, size_t size) {
    (void)dchid;
    memcpy(sent[sent_count], msg, size);
    sent_size[sent_count] = size;
    if (size == 9)
        trace[trace_len++] = ((unsigned char *)msg)[4] ? 'h' : 'w';
    return sent_count++ == fail_at ? -5 : (int)size;
}
static struct apple_smc *dchid_afe_smc(struct dchid_iface *iface) {
    (void)iface;
    return smc_present ? (struct apple_smc *)&sent : NULL;
}
static int apple_smc_read_u8(struct apple_smc *smc, u32 key, u8 *p) {
    (void)smc; (void)key;
    if (smc_fail) return -5;
    *p = smc_value;
    return 0;
}
static int apple_smc_write_u8(struct apple_smc *smc, u32 key, u8 v) {
    (void)smc; (void)key;
    if (smc_fail) return -5;
    smc_value = v;
    trace[trace_len++] = v ? 'P' : 'p';
    return 0;
}
static int gpiod_direction_output_raw(struct gpio_desc *desc, int value) {
    (void)desc;
    if (gpio_fail) return -5;
    afe_level = value;
    trace[trace_len++] = value ? '1' : '0';
    return 0;
}
static int gpiod_get_raw_value_cansleep(const struct gpio_desc *desc) {
    (void)desc;
    return afe_level;
}
static void fsleep(unsigned int us) { slept_us += us; }
static ktime_t ktime_get(void) { return 0; }
static long long ktime_us_delta(ktime_t a, ktime_t b) { return a - b; }
static void reset_capture(void) {
    memset(sent, 0, sizeof(sent)); memset(sent_size, 0, sizeof(sent_size));
    memset(trace, 0, sizeof(trace));
    sent_count = 0; fail_at = -1; slept_us = 0; trace_len = 0; gpio_fail = 0;
    smc_present = 1; smc_fail = 0; smc_value = 1; afe_level = 1;
    ipd_reset_sequence = true; ipd_reset_write = true;
    power_method = -1;
    afe_enable_level = 0; afe_disable_level = 1;
}
'''
        cases = r'''
int main(void) {
    struct gpio_desc *afe = (struct gpio_desc *)&main;
    struct device_node parent = {0}, child = {0};
    struct device device = { .of_node = &parent };
    struct dockchannel_hid dchid = { .dev = &device };
    struct dchid_iface touch = {
        .dchid = &dchid, .index = 3, .name = "multi-touch", .of_node = &child,
        .afe_reset = afe,
    };

    /* Non-J700 platforms keep the legacy single-message form and no platform work. */
    reset_capture();
    assert(dchid_reset_interface(&touch, 2, true) == 4);
    assert(sent_count == 1 && sent_size[0] == 4 && slept_us == 0);
    assert(memcmp(sent[0], (u8[]){0x40, 1, 3, 2}, 4) == 0);
    assert(trace_len == 0);

    parent.enabled = true; child.firmware = "apple/tpmtfw-j700.bin";

    /*
     * Power off asserts the pmIP reset first with no delay, then removes the
     * gP13 supply and holds it for the personality's 50 ms.
     */
    reset_capture();
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_OFF, true) == 9);
    assert(sent_count == 2 && sent_size[0] == 9 && sent_size[1] == 9);
    assert(slept_us == DCHID_AFE_RESET_DISABLE_DELAY_US);
    assert(strcmp(trace, "wP1h") == 0);
    assert(memcmp(sent[0], (u8[]){0x40,2,3,0,0,0,0,0,0}, 9) == 0);
    assert(memcmp(sent[1], (u8[]){0x40,2,3,0,1,0,0,0,0}, 9) == 0);

    /*
     * Power on raises the supply first with no delay, then releases the pmIP
     * reset and waits the personality's 100 ms before has-changed.
     */
    reset_capture();
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON, true) == 9);
    assert(sent_count == 2);
    assert(slept_us == DCHID_IPD_RESET_ENABLE_DELAY_US);
    assert(strcmp(trace, "w0ph") == 0);
    assert(memcmp(sent[0], (u8[]){0x40,2,3,2,0,0,0,0,0}, 9) == 0);
    assert(memcmp(sent[1], (u8[]){0x40,2,3,2,1,0,0,0,0}, 9) == 0);

    /* Both settles are the sequence's own delay, never an invented one. */
    assert(DCHID_AFE_RESET_ENABLE_DELAY_US == 0);
    assert(DCHID_IPD_RESET_DISABLE_DELAY_US == 0);

    /* The switchable levels default to exactly what the personality decodes to. */
    assert(afe_enable_level == DCHID_AFE_RESET_ENABLE_LEVEL);
    assert(afe_disable_level == DCHID_AFE_RESET_DISABLE_LEVEL);

    /* Inverting them swaps only the driven levels, not the order or the delays. */
    reset_capture(); afe_enable_level = 1; afe_disable_level = 0;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON, true) == 9);
    assert(strcmp(trace, "w1ph") == 0);
    assert(slept_us == DCHID_IPD_RESET_ENABLE_DELAY_US);
    reset_capture(); afe_enable_level = 1; afe_disable_level = 0;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_OFF, true) == 9);
    assert(strcmp(trace, "wP0h") == 0);
    assert(slept_us == DCHID_AFE_RESET_DISABLE_DELAY_US);

    /* Out-of-range levels are clamped rather than passed through. */
    reset_capture(); afe_enable_level = 7;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON, true) == 9);
    assert(strcmp(trace, "w1ph") == 0);

    /* A failed will-change never touches the platform. */
    reset_capture(); fail_at = 0;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_OFF, true) == -5);
    assert(sent_count == 1 && strcmp(trace, "w") == 0);

    /* A failed supply write aborts before has-changed. */
    reset_capture(); gpio_fail = 1;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON, true) == -5);
    assert(sent_count == 1 && slept_us == 0 && strcmp(trace, "w") == 0);

    /* A missing supply line fails closed rather than half-completing. */
    reset_capture(); touch.afe_reset = NULL;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON, true) == -ENODEV);
    assert(sent_count == 1 && strcmp(trace, "w") == 0);
    touch.afe_reset = afe;

    /*
     * An unreachable SMC is not fatal: the transition still completes and still
     * applies the reset-sequence delay, so the boot still yields MTP's verdict.
     */
    reset_capture(); smc_present = 0;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON, true) == 9);
    assert(sent_count == 2 && slept_us == DCHID_IPD_RESET_ENABLE_DELAY_US);
    assert(strcmp(trace, "w0h") == 0);

    /* A failing SMC write is likewise reported but not fatal. */
    reset_capture(); smc_fail = 1;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON, true) == 9);
    assert(sent_count == 2 && strcmp(trace, "w0h") == 0);

    /* ipd_reset_write=0 keeps the delay and drops only the key write. */
    reset_capture(); ipd_reset_write = false;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON, true) == 9);
    assert(slept_us == DCHID_IPD_RESET_ENABLE_DELAY_US);
    assert(strcmp(trace, "w0h") == 0);

    /* ipd_reset_sequence=0 restores the supply-only behaviour exactly. */
    reset_capture(); ipd_reset_sequence = false;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON, true) == 9);
    assert(slept_us == 0 && strcmp(trace, "w0h") == 0);
    reset_capture(); ipd_reset_sequence = false;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_OFF, true) == 9);
    assert(slept_us == DCHID_AFE_RESET_DISABLE_DELAY_US);
    assert(strcmp(trace, "w1h") == 0);

    /* Other firmware and other interfaces keep the legacy behaviour. */
    child.firmware = "apple/tpmtfw-other.bin"; reset_capture();
    assert(dchid_reset_interface(&touch, 2, true) == 4);
    assert(sent_count == 1 && sent_size[0] == 4 && trace_len == 0);

    child.firmware = "apple/tpmtfw-j700.bin";
    touch.name = "keyboard"; reset_capture();
    assert(dchid_reset_interface(&touch, 2, true) == 4);
    assert(sent_count == 1 && sent_size[0] == 4 && trace_len == 0);
    return 0;
}
'''

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "j700_power.c"
            binary = Path(tmp) / "j700_power"
            path.write_text(harness + constants + "\n" + functions + cases)
            subprocess.run(
                [compiler, "-fsanitize=address,undefined", "-fno-sanitize-recover=all",
                 "-Wall", "-Werror", "-o", str(binary), str(path)],
                check=True,
            )
            subprocess.run([str(binary)], check=True)


if __name__ == "__main__":
    unittest.main()

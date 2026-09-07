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
            if re.match(r"#define (CMD_RESET_INTERFACE|DCHID_POWER_|DCHID_AFE_)\w*\s", line)
        )
        start = source.index("static bool dchid_uses_transactional_power(")
        end = source.index("\nstatic int dchid_send_firmware", start)
        functions = source[start:end]

        harness = r'''
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
typedef uint8_t u8;
struct device_node { bool enabled; const char *firmware; };
struct device { struct device_node *of_node; };
struct dockchannel_hid { struct device *dev; };
struct gpio_desc;
struct dchid_iface {
    struct dockchannel_hid *dchid;
    int index;
    const char *name;
    const struct device_node *of_node;
    struct gpio_desc *afe_reset;
};
#define ENODEV 19
#define dev_err(dev, ...) ((void)(dev))
#define dev_info(dev, ...) ((void)(dev))
static unsigned char sent[4][16];
static size_t sent_size[4];
static int sent_count, fail_at = -1, slept_us;
/* Interleaved trace: 'w' will-change, 'h' has-changed, '0'/'1' raw AFE level. */
static char trace[16];
static int trace_len;
static int gpio_fail;
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
static int gpiod_direction_output_raw(struct gpio_desc *desc, int value) {
    (void)desc;
    if (gpio_fail) return -5;
    trace[trace_len++] = value ? '1' : '0';
    return 0;
}
static void fsleep(unsigned int us) { slept_us += us; }
static void reset_capture(void) {
    memset(sent, 0, sizeof(sent)); memset(sent_size, 0, sizeof(sent_size));
    memset(trace, 0, sizeof(trace));
    sent_count = 0; fail_at = -1; slept_us = 0; trace_len = 0; gpio_fail = 0;
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

    /* Non-J700 platforms keep the legacy single-message form and no GPIO. */
    assert(dchid_reset_interface(&touch, 2) == 4);
    assert(sent_count == 1 && sent_size[0] == 4 && slept_us == 0);
    assert(memcmp(sent[0], (u8[]){0x40, 1, 3, 2}, 4) == 0);
    assert(trace_len == 0);

    parent.enabled = true; child.firmware = "apple/tpmtfw-j700.bin";

    /* Power off asserts the AFE reset between the phases and holds it. */
    reset_capture();
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_OFF) == 9);
    assert(sent_count == 2 && sent_size[0] == 9 && sent_size[1] == 9);
    assert(slept_us == DCHID_AFE_RESET_HOLD_US);
    assert(strcmp(trace, "w1h") == 0);
    assert(memcmp(sent[0], (u8[]){0x40,2,3,0,0,0,0,0,0}, 9) == 0);
    assert(memcmp(sent[1], (u8[]){0x40,2,3,0,1,0,0,0,0}, 9) == 0);

    /* Power on releases it with no delay, still between the phases. */
    reset_capture();
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON) == 9);
    assert(sent_count == 2 && slept_us == 0);
    assert(strcmp(trace, "w0h") == 0);
    assert(memcmp(sent[0], (u8[]){0x40,2,3,2,0,0,0,0,0}, 9) == 0);
    assert(memcmp(sent[1], (u8[]){0x40,2,3,2,1,0,0,0,0}, 9) == 0);

    /* A failed will-change never touches the line. */
    reset_capture(); fail_at = 0;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_OFF) == -5);
    assert(sent_count == 1 && strcmp(trace, "w") == 0);

    /* A failed line write aborts before has-changed. */
    reset_capture(); gpio_fail = 1;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_OFF) == -5);
    assert(sent_count == 1 && slept_us == 0 && strcmp(trace, "w") == 0);

    /* A missing line fails closed rather than half-completing. */
    reset_capture(); touch.afe_reset = NULL;
    assert(dchid_reset_interface(&touch, DCHID_POWER_STATE_ON) == -ENODEV);
    assert(sent_count == 1 && strcmp(trace, "w") == 0);
    touch.afe_reset = afe;

    /* Other firmware and other interfaces keep the legacy behaviour. */
    child.firmware = "apple/tpmtfw-other.bin"; reset_capture();
    assert(dchid_reset_interface(&touch, 2) == 4);
    assert(sent_count == 1 && sent_size[0] == 4 && trace_len == 0);

    child.firmware = "apple/tpmtfw-j700.bin";
    touch.name = "keyboard"; reset_capture();
    assert(dchid_reset_interface(&touch, 2) == 4);
    assert(sent_count == 1 && sent_size[0] == 4 && trace_len == 0);
    return 0;
}
'''

        with tempfile.TemporaryDirectory() as directory:
            c_file = Path(directory) / "power.c"
            binary = Path(directory) / "power"
            c_file.write_text(constants + "\n" + harness + functions + cases)
            subprocess.run(
                [compiler, "-std=c11", "-Wall", "-Wextra", "-Werror",
                 "-fsanitize=address,undefined", str(c_file), "-o", str(binary)],
                check=True,
            )
            subprocess.run([str(binary)], check=True, timeout=30)


if __name__ == "__main__":
    unittest.main()

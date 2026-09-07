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
            if re.match(r"#define (CMD_RESET_INTERFACE|DCHID_POWER_)\w*\s", line)
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
struct dchid_iface {
    struct dockchannel_hid *dchid;
    int index;
    const char *name;
    const struct device_node *of_node;
};
static unsigned char sent[4][16];
static size_t sent_size[4];
static int sent_count, fail_at = -1, slept_ms;
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
    return sent_count++ == fail_at ? -5 : (int)size;
}
static void msleep(unsigned int ms) { slept_ms += ms; }
static void reset_capture(void) {
    memset(sent, 0, sizeof(sent)); memset(sent_size, 0, sizeof(sent_size));
    sent_count = 0; fail_at = -1; slept_ms = 0;
}
'''
        cases = r'''
int main(void) {
    struct device_node parent = {0}, child = {0};
    struct device device = { .of_node = &parent };
    struct dockchannel_hid dchid = { .dev = &device };
    struct dchid_iface touch = {
        .dchid = &dchid, .index = 3, .name = "multi-touch", .of_node = &child,
    };

    assert(dchid_reset_interface(&touch, 2) == 4);
    assert(sent_count == 1 && sent_size[0] == 4 && slept_ms == 0);
    assert(memcmp(sent[0], (u8[]){0x40, 1, 3, 2}, 4) == 0);

    parent.enabled = true; child.firmware = "apple/tpmtfw-j700.bin";
    reset_capture();
    assert(dchid_reset_interface(&touch, 0) == 9);
    assert(sent_count == 2 && sent_size[0] == 9 && sent_size[1] == 9);
    assert(slept_ms == 0);
    assert(memcmp(sent[0], (u8[]){0x40,2,3,0,0,0,0,0,0}, 9) == 0);
    assert(memcmp(sent[1], (u8[]){0x40,2,3,0,1,0,0,0,0}, 9) == 0);

    reset_capture();
    assert(dchid_reset_interface(&touch, 2) == 9);
    assert(sent_count == 2 && slept_ms == 50);
    assert(memcmp(sent[0], (u8[]){0x40,2,3,2,0,0,0,0,0}, 9) == 0);
    assert(memcmp(sent[1], (u8[]){0x40,2,3,2,1,0,0,0,0}, 9) == 0);

    reset_capture(); fail_at = 0;
    assert(dchid_reset_interface(&touch, 0) == -5);
    assert(sent_count == 1);

    child.firmware = "apple/tpmtfw-other.bin"; reset_capture();
    assert(dchid_reset_interface(&touch, 2) == 4);
    assert(sent_count == 1 && sent_size[0] == 4);

    child.firmware = "apple/tpmtfw-j700.bin";
    touch.name = "keyboard"; reset_capture();
    assert(dchid_reset_interface(&touch, 2) == 4);
    assert(sent_count == 1 && sent_size[0] == 4);
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

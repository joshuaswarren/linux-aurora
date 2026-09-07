# SPDX-License-Identifier: GPL-2.0
"""Exercise the DockChannel shared-memory ring primitives in userspace."""

from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[5]


class DockChannelFifoTest(unittest.TestCase):
    def test_wraparound_copy_and_readable_count(self):
        compiler = shutil.which("clang") or shutil.which("cc")
        if compiler is None:
            self.skipTest("C compiler required")

        source = (ROOT / "drivers/hid/dockchannel-hid/dockchannel-hid.c").read_text()
        start = source.index("static size_t dchid_fifo_readable(")
        end = source.index("\nstruct dchid_init_block_hdr", start)
        helpers = source[start:end]

        harness = r'''
#include <assert.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
typedef uint32_t u32;
typedef uint8_t u8;
#define min(a, b) ((a) < (b) ? (a) : (b))
'''
        cases = r'''
int main(void)
{
    static const unsigned char ring[16] = {
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    };
    unsigned char out[12] = {};

    assert(dchid_fifo_readable(9, 2, 16) == 7);
    assert(dchid_fifo_readable(2, 14, 16) == 4);
    assert(dchid_fifo_readable(7, 7, 16) == 0);
    assert(dchid_fifo_readable(16, 0, 16) == 0);
    assert(dchid_fifo_readable(0, 16, 16) == 0);

    dchid_fifo_copy(out, ring, 16, 3, 8);
    assert(memcmp(out, (unsigned char[]){3,4,5,6,7,8,9,10}, 8) == 0);

    memset(out, 0, sizeof(out));
    dchid_fifo_copy(out, ring, 16, 13, 7);
    assert(memcmp(out, (unsigned char[]){13,14,15,0,1,2,3}, 7) == 0);

    /* A minimum 12-byte SCM message may itself straddle the ring end. */
    memset(out, 0, sizeof(out));
    dchid_fifo_copy(out, ring, 16, 10, sizeof(out));
    assert(memcmp(out,
                  (unsigned char[]){10,11,12,13,14,15,0,1,2,3,4,5},
                  sizeof(out)) == 0);
    return 0;
}
'''

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "dockchannel_fifo.c"
            binary = Path(tmp) / "dockchannel_fifo"
            path.write_text(harness + helpers + cases)
            subprocess.run(
                [compiler, "-fsanitize=address,undefined",
                 "-fno-sanitize-recover=all", "-Wall", "-Werror",
                 "-o", str(binary), str(path)],
                check=True,
            )
            subprocess.run([str(binary)], check=True)


if __name__ == "__main__":
    unittest.main()

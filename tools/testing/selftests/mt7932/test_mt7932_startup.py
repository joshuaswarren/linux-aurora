#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Fault-inject the real automatic initialization sequencing with host mocks."""
from pathlib import Path
import os
import subprocess
import tempfile

root = Path(__file__).resolve().parents[4]
source = (root / 'drivers/net/wireless/mediatek/mt7932/mt7932.c').read_text()
def function(name):
    start = source.index('static int ' + name + '(struct mt7932 *m)\n{')
    return source[start:source.index('\n}\n', start) + 3]

for obsolete in ('firmware_test', 'config_test', 'recovery_test', 'board_test',
                 'precal_test', 'startup_test', 'buffer_test'):
    assert obsolete not in source

code = '''#include <assert.h>
#include <errno.h>
#include <stdio.h>
struct mt7932 { int unused; };
static int step, fail;
static int run(int expected) {
    assert(++step == expected);
    return step == fail ? -ETIMEDOUT : 0;
}
#define STAGE(name, id) static int name(struct mt7932 *m) { (void)m; return run(id); }
STAGE(mt_dma_setup, 1)
STAGE(mt_rom_gate, 2)
STAGE(mt_capability_gate, 4)
STAGE(mt_config_gate, 5)
STAGE(mt_recovery_gate, 6)
STAGE(mt_board_gate, 7)
STAGE(mt_precal_gate, 8)
STAGE(mt_register_regulatory_gate, 9)
'''
code += function('mt_runtime_start')
code += '''
static int mt_firmware_gate(struct mt7932 *m) {
    int ret = run(3);
    return ret ? ret : mt_runtime_start(m);
}
'''
code += function('mt_initialize')
code += '''
int main(void) {
    struct mt7932 m = {};
    for (fail = 0; fail <= 9; fail++) {
        step = 0;
        assert(mt_initialize(&m) == (fail ? -ETIMEDOUT : 0));
        assert(step == (fail ? fail : 9));
    }
    puts("PASS: automatic Wi-Fi initialization and all nine failure boundaries");
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-startup-') as tmp:
    path = Path(tmp) / 'test.c'
    binary = Path(tmp) / 'test'
    path.write_text(code)
    subprocess.run([os.environ.get('CC', 'cc'), '-O1', '-g', '-Wall', '-Wextra', '-Werror',
                    '-fsanitize=address,undefined', '-fno-pie', '-no-pie',
                    str(path), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)

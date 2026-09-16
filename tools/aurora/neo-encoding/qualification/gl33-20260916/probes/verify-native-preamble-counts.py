#!/usr/bin/env python3
"""Compare production count conversion with native CPU arithmetic, never a GPU.

Only six checked arithmetic instructions from the digest-pinned HAL200 driver
are executed, inside ordinary compiled host functions with explicit inputs.
No driver entry point, pointer load/store, branch target, or MMIO is executed.
"""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import tempfile

from native_metal import NativeMetal

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3] / 'mesa-j700-preamble-20260915'


def assembly(reader):
    fragments = [
        ('native_ipr_count', 'w12', 0x216b8b644, 0x216b8b654,
         [('add', 'w9, w12, #3'), ('lsr', 'w9, w9, #2'),
          ('add', 'w9, w9, #1'), ('and', 'w9, w9, #0x7ffffffe')]),
        ('native_spill_count', 'w10', 0x216b8b638, 0x216b8b640,
         [('add', 'w9, w10, #3'), ('lsr', 'w9, w9, #2')]),
    ]
    lines, records = ['.text'], []
    for name, input_register, start, end, expected in fragments:
        instructions = reader.disassemble(start, end)
        assert [(x.mnemonic, x.op_str) for x in instructions] == expected
        lines += [f'.globl _{name}', '.p2align 2', f'_{name}:',
                  f'mov {input_register}, w0']
        for ins in instructions:
            word = int.from_bytes(ins.bytes, 'little')
            lines.append(f'.long 0x{word:08x}')
            records.append({'address': hex(ins.address), 'bytes': ins.bytes.hex(),
                            'mnemonic': ins.mnemonic, 'operands': ins.op_str})
        lines += ['mov w0, w9', 'ret']
    return '\n'.join(lines) + '\n', records


HARNESS = r'''
#include "agx_vdm_state_update_core.h"
#include <stdio.h>
#include <string.h>
extern uint32_t native_ipr_count(uint32_t bytes);
extern uint32_t native_spill_count(uint32_t bytes);
static unsigned checked;
static int check(uint32_t ipr, uint32_t spill) {
   struct agx_vdm_state_update_preamble_counts out;
   if (!agx_vdm_state_update_encode_preamble_counts(AGX_CHIP_G17P, ipr, spill, &out) ||
       out.ipr_dwords != native_ipr_count(ipr) ||
       out.spill_dwords != native_spill_count(spill)) {
      fprintf(stderr, "native mismatch: ipr=%u spill=%u\n", ipr, spill);
      return 1;
   }
   checked++;
   return 0;
}
int main(void) {
   /* Exhaust each legal byte domain, with mixed values in the other field. */
   for (uint32_t ipr = 0; ipr <= 16376; ++ipr)
      if (check(ipr, (ipr * 64u + 1u) % 1048573u)) return 1;
   for (uint32_t spill = 0; spill <= 1048572; ++spill)
      if (check((spill * 17u) % 16377u, spill)) return 1;
   const uint32_t invalid[][2] = {
      {16377, 0}, {16384, 0}, {0, 1048573}, {0, 1048576},
      {UINT32_MAX, 0}, {0, UINT32_MAX}, {UINT32_MAX, UINT32_MAX},
   };
   for (unsigned i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
      struct agx_vdm_state_update_preamble_counts out, zero;
      memset(&zero, 0, sizeof(zero));
      memset(&out, 0xff, sizeof(out));
      if (agx_vdm_state_update_encode_preamble_counts(
             AGX_CHIP_G17P, invalid[i][0], invalid[i][1], &out) ||
          memcmp(&out, &zero, sizeof(out))) return 1;
   }
   printf("%u native arithmetic comparisons; 7 rejection/zero-output controls; PASS\n", checked);
   return 0;
}
'''


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    assert platform.system() == 'Darwin' and platform.machine() == 'arm64'
    reader = NativeMetal()
    asm, records = assembly(reader)
    sources = [ROOT / 'src/asahi/lib/agx_vdm_state_update_core.c',
               ROOT / 'src/asahi/lib/agx_vdm_state_core.c']
    inputs = sources + [ROOT / 'src/asahi/lib' / name for name in
                        ('agx_vdm_state_update_core.h', 'agx_vdm_state_core.h', 'agx_chip.h')]
    hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in inputs}
    with tempfile.TemporaryDirectory(prefix='j700-native-counts-') as temp:
        temp = Path(temp)
        (temp / 'native.S').write_text(asm)
        (temp / 'harness.c').write_text(HARNESS)
        command = ['clang', '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror',
                   '-fsanitize=undefined', '-fno-sanitize-recover=all',
                   '-I' + str(ROOT / 'include'), '-I' + str(ROOT / 'src/asahi/lib'),
                   *map(str, sources), str(temp / 'native.S'), str(temp / 'harness.c'),
                   '-o', str(temp / 'verify')]
        build = subprocess.run(command, capture_output=True, text=True, check=True)
        run = subprocess.run([str(temp / 'verify')], capture_output=True, text=True,
                             timeout=30, check=True)
    args.output.write_text(json.dumps({
        'status': 'pass', 'native_binary_sha256': reader.sha256,
        'source_sha256': hashes, 'native_instructions': records,
        'assembly_sha256': hashlib.sha256(asm.encode()).hexdigest(),
        'harness_sha256': hashlib.sha256(HARNESS.encode()).hexdigest(),
        'build_command': command, 'build_log': build.stdout + build.stderr,
        'result': run.stdout, 'stderr': run.stderr,
        'scope': 'CPU arithmetic/field bounds only; no GPU or nonzero-IPR runtime qualification.',
    }, indent=2) + '\n')
    print(run.stdout.strip())

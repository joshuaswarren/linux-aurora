"""Replay the captured USB DART replica mismatch through production C."""
from pathlib import Path
import re
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parent / 'm1n1'
source = (Path(sys.argv[1]) if len(sys.argv) > 1 else root / 'src/sptm/dart.c').read_text()
function = source.split('static inline u32 dart_c_finalize_replay(', 1)[1]
function = 'static inline u32 dart_c_finalize_replay(' + function.split('\nstatic inline ', 1)[0]
# Replace only the architecture barrier for the host RAM mock.
function = function.replace('__asm__ volatile("dsb sy" ::: "memory");', '(void)0;')
header = (root / 'src/sptm/private.h').read_text()
sid = 'struct dart_c_sid_state {' + header.split('struct dart_c_sid_state {', 1)[1].split('};', 1)[0] + '};'
macros = '\n'.join(l for l in header.splitlines() if any(l.startswith('#define ' + n + ' ') for n in (
    'DART_C_MAX_BASES', 'DART_C_MAX_SIDS', 'DART_T8110_TCR', 'DART_T8110_TTBR',
    'DART_T8110_TTBR_VALID', 'DART_T8110_TCR_REMAP_EN', 'DART_T8110_TCR_BYPASS_DART',
    'DART_T8110_TCR_BYPASS_DAPF', 'DART_T8110_ENABLE_STREAMS')))
harness = r'''
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
typedef uint64_t u64;
typedef uint32_t u32;
typedef uint8_t u8;
typedef int bool_t;
@@MACROS@@
@@SID@@
struct dart_c_instance { u64 base_pa[DART_C_MAX_BASES]; u32 base_count;
    struct dart_c_sid_state sid[DART_C_MAX_SIDS]; };
static u32 regs[2][0x2000 / 4], flushes[256], mmio_writes;
static u32 *reg(u64 addr) {
    u64 base = addr >= 0x402f80000UL ? 0x402f80000UL : 0x402f00000UL;
    assert(addr >= base && addr < base + 0x2000 && !(addr & 3));
    return &regs[base == 0x402f80000UL][(addr - base) / 4];
}
static u32 dart_c_read32(u64 a) { return *reg(a); }
static void dart_c_write32(u64 a, u32 v) { *reg(a) = v; mmio_writes++; }
static bool_t dart_c_looks_like_mmio(u64 a) { return a == 0x402f00000UL || a == 0x402f80000UL; }
static bool_t dart_c_sid_exclave(struct dart_c_sid_state *s) { return s->exclave; }
static u32 dart_c_num_streams(u64 a) { (void)a; return 16; }
static u32 dart_c_stream_words(u32 n) { return (n + 31) / 32; }
static void dart_c_for_each_base_flush_sid(struct dart_c_instance *i, u32 sid) { (void)i; flushes[sid]++; }
@@FUNCTION@@
int main(void) {
    struct dart_c_instance inst = {.base_pa={0x402f00000UL,0x402f80000UL}, .base_count=2};
    inst.sid[14] = (struct dart_c_sid_state){.valid=1, .tcr=0, .ttbr=0x247f5f74};
    regs[0][(0x1000+14*4)/4]=1;
    regs[0][(0x1400+14*4)/4]=0x3fec96bc;
    regs[1][(0x1400+14*4)/4]=0x247f5f74;
    inst.sid[1] = (struct dart_c_sid_state){.valid=1,.root_valid=1,.tcr=1,.ttbr=0x100064b9,.stream_enabled=1};
    inst.sid[2] = (struct dart_c_sid_state){.valid=1,.raw_replay=1,.tcr=1,.ttbr=0x07f9bf3c};
    inst.sid[13] = (struct dart_c_sid_state){.valid=1,.exclave=1,.tcr=0,.ttbr=0};
    for (u32 b=0;b<2;b++) {
        regs[b][(0x1400+13*4)/4]=0x1230+b*4;
        regs[b][(0x1400+15*4)/4]=0x5670+b*4;
    }
    dart_c_finalize_replay(&inst);
    for (u32 b=0;b<2;b++) {
        assert(regs[b][(0x1400+14*4)/4] == 0x247f5f74);
        assert(regs[b][(0x1000+14*4)/4] == 0);
        assert(regs[b][DART_T8110_ENABLE_STREAMS/4] == (1U<<1));
        assert(regs[b][(0x1400+1*4)/4] == 0x100064b9);
        assert(regs[b][(0x1400+2*4)/4] == 0x07f9bf3c);
        assert(regs[b][(0x1400+13*4)/4] == 0x1230+b*4);
        assert(regs[b][(0x1400+15*4)/4] == 0x5670+b*4);
    }
    assert(flushes[14] == 1 && flushes[13] == 0 && flushes[15] == 0);
    u32 before = mmio_writes;
    dart_c_finalize_replay(&inst);
    assert(mmio_writes == before + 2); /* only the two enable bitmaps */
    puts("DART replay: captured SID14 mismatch repaired, streams stay disabled, roots/raw/exclave/unknown cases pass");
}
'''.replace('@@MACROS@@', macros).replace('@@SID@@', sid).replace('@@FUNCTION@@', function)
with tempfile.TemporaryDirectory(prefix='m5-dart-replay-') as directory:
    path = Path(directory)
    (path / 'test.c').write_text(harness)
    subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-Werror', str(path / 'test.c'),
                    '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)

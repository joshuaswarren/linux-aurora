#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Compile the real Linux channel/DAPF functions with instrumented host MMIO.

Optional raw IPC metadata lets the channel test consume a saved native table.
The default fixture contains only observed transport metadata, no firmware.
"""
import argparse
import importlib.util
from pathlib import Path
import struct
import subprocess
import tempfile


def function(source, name):
    start = source.index("static int " + name + "(")
    brace = source.index("{", start)
    level = 1
    end = brace + 1
    while level:
        level += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ipc-capture", type=Path)
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    source = (here.parents[2] / "drivers/soc/apple/apple-ane-fw-test.c").read_text()
    ipc_source = (here.parents[2] / "drivers/soc/apple/apple-ane-ipc.h").read_text()
    if args.ipc_capture:
        table = args.ipc_capture.read_bytes()[:0x800]
    else:
        spec = importlib.util.spec_from_file_location("metadata_fixture", here / "test-ipc.py")
        fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fixture)
        table = fixture.table(0x100003d0000)
    if len(table) != 0x800:
        raise ValueError("expected all eight native descriptors")
    constants = '\n'.join(x for x in source.split('struct ane_buffer {')[0].splitlines()
                          if x.startswith('#define ANE_'))
    layouts = source[source.index('struct ane_channel {'):source.index('static const phys_addr_t')]
    slot = ipc_source[ipc_source.index('struct apple_ane_ipc_slot {'):ipc_source.index('struct apple_ane_io_transaction')]
    fixture = ','.join(str(x) for x in table)
    prelude = r'''
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <errno.h>
typedef uint8_t u8; typedef uint32_t u32; typedef uint64_t u64; typedef uint64_t __le64;
#define ARRAY_SIZE(x) (sizeof(x) / sizeof((x)[0]))
#define BIT(x) (1U << (x))
#define lower_32_bits(x) ((u32)(x))
#define upper_32_bits(x) ((u32)((x) >> 32))
#define WRITE_ONCE(x, y) ((x) = (y))
#define cpu_to_le64(x) (x)
#define dma_rmb() ((void)0)
#define dma_wmb() ((void)0)
#define dev_info(...) ((void)0)
static u32 get_unaligned_le32(const void *p) { u32 x; memcpy(&x,p,4); return x; }
static u64 get_unaligned_le64(const void *p) { u64 x; memcpy(&x,p,8); return x; }
static unsigned int writes, reject_write;
static u32 readl(const void *p) { return get_unaligned_le32(p); }
static void writel(u32 v, void *p) { writes++; if (writes != reject_write) memcpy(p,&v,4); }
struct ane_buffer { void *cpu; };
struct ane_fw_test { void *trad[3]; void *dapf; struct ane_buffer *arena; };
'''
    test = r'''
static void put32(void *p,u32 x) { memcpy(p,&x,4); }
int main(void) {
    struct ane_buffer arena = { .cpu = calloc(1,ANE_ARENA_SIZE) };
    u8 trad[0x400] = {0}, dapf[0x100] = {0};
    struct ane_fw_test ane = { .trad = {trad}, .dapf = dapf, .arena = &arena };
    assert(arena.cpu);
    u8 *ipc = arena.cpu + ANE_IPC;
    memcpy(ipc,native_table,sizeof(native_table));
    assert(ane_validate_channels(&ane) == 0);
    for (unsigned int i=0;i<8;i++) {
        const struct ane_channel *c=&ane_channels[i];
        struct apple_ane_ipc_slot *s=(void *)(ipc+c->offset);
        for (unsigned int j=0;j<c->count;j++) assert(s[j].pointer == (c->direction ? 0 : 1));
    }
    // Corrupt every descriptor field in turn; failure must precede all ring writes.
    const unsigned int fields[] = {0,0x40,0x44,0x48,0x50};
    for (unsigned int i=0;i<8;i++) for(unsigned int j=0;j<ARRAY_SIZE(fields);j++) {
        memset(ipc,0x5a,0x10000); memcpy(ipc,native_table,sizeof(native_table));
        ipc[i*0x100+fields[j]] ^= 1;
        assert(ane_validate_channels(&ane) == -EPROTO);
        for (unsigned int k=0x800;k<0x10000;k++) assert(ipc[k]==0x5a);
    }
    put32(trad+8,0x203); put32(trad+12,16); put32(trad+0x200,6); put32(trad+0x208,6);
    writes=0; assert(ane_prepare_dapf(&ane)==-EPERM); assert(writes==0);
    put32(trad+0x200,2); put32(trad+0x208,2);
    assert(ane_prepare_dapf(&ane)==0); assert(writes==24);
    assert(readl(trad+0x200)==2 && readl(trad+0x208)==2);
    for(unsigned int i=0;i<4;i++) assert(readl(dapf+i*0x40+4)==0);
    writes=0; assert(ane_prepare_dapf(&ane)==0); assert(writes==0);
    put32(trad+0x200,6); put32(trad+0x208,6);
    assert(ane_prepare_dapf(&ane)==0); assert(writes==0);
    // Unsupported hardware and failed readback never receive a success result.
    put32(trad+8,0x204); assert(ane_prepare_dapf(&ane)==-EOPNOTSUPP); assert(writes==0);
    put32(trad+8,0x203); put32(trad+12,64); assert(ane_prepare_dapf(&ane)==-EOPNOTSUPP);
    put32(trad+12,16); put32(trad+0x200,2); put32(trad+0x208,2);
    memset(dapf,0,sizeof(dapf)); writes=0; reject_write=1;
    assert(ane_prepare_dapf(&ane)==-EIO); assert(writes==1);
    free(arena.cpu);
    puts("ANE_LINUX_FW_C_TESTS_PASS: native channels, 40 corruptions, DAPF lock/write/readback bounds");
    return 0;
}
'''
    program = prelude + constants + '\n' + layouts + slot
    program += f'\nstatic const u8 native_table[] = {{{fixture}}};\n'
    program += function(source, 'ane_validate_channels') + '\n' + function(source, 'ane_prepare_dapf') + test
    with tempfile.TemporaryDirectory(prefix='ane-fw-c-test-') as temporary:
        cfile, executable = Path(temporary) / 'test.c', Path(temporary) / 'test'
        cfile.write_text(program)
        subprocess.run(['clang', '-std=gnu11', '-Wall', '-Wextra', '-Werror',
                        '-fsanitize=address,undefined', '-g', str(cfile), '-o', str(executable)], check=True)
        subprocess.run([str(executable)], check=True)


if __name__ == '__main__':
    main()

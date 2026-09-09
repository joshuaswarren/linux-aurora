"""Exercise production PTE accounting across permission changes and teardown."""
from pathlib import Path
import re
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parent / 'm1n1'
header = (Path(sys.argv[1]) if len(sys.argv) > 1 else
          root / 'src/sptm/private.h').read_text()


def function(name):
    start = re.search(r'^static inline [^\n]+\b' + name + r'\(', header, re.M).start()
    opening = header.index('{', start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (header[end] == '{') - (header[end] == '}')
        end += 1
    return header[start:end]


names = ('ARM_PTE_PAGE_MASK', 'ARM_PTE_PAGE_MASK_4K', 'ARM_PTE_AP_RO',
         'ARM_PTE_PNX', 'ARM_PTE_NX', 'ARM_PTE_WRITEABLE',
         'FT_DATA_RO_REFCNT_OFF', 'FT_DATA_WX_REFCNT_OFF',
         'FT_KIND5_REFCNT_OFF', 'XNU_PAGE_TABLE_ALT')
macros = '\n'.join(line for line in header.splitlines()
                   if any(re.match(r'#define\s+' + name + r'\s', line) for name in names))
functions = '\n'.join(function(name) for name in (
    'pte_uses_wx_refcount', 'ft_inc_page_refcount_for_pte',
    'ft_dec_page_refcount_for_pte', 'update_refcounts_for_pte_change_geom'))
harness = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
typedef uint64_t u64;
typedef uint32_t u32;
typedef uint16_t u16;
typedef uint8_t u8;
typedef int bool_t;
@@MACROS@@
#define BASE 0x103b9844000UL
static u32 refs[4][4];
static u8 types[4];
static unsigned index_for(u64 pa) {
    assert(pa >= BASE && pa < BASE + 4 * 0x4000);
    return (pa - BASE) >> 14;
}
static u8 ft_get_type(u64 pa) { return types[index_for(pa)]; }
static bool_t is_pt_type(u8 type) { return type == 99; }
static void ft_inc_u32(u64 pa, u64 off) { refs[index_for(pa)][off / 4]++; }
static void ft_dec_u32(u64 pa, u64 off) {
    u32 *count = &refs[index_for(pa)][off / 4];
    if (*count) (*count)--;
}
static void ft_inc_u16(u64 pa, u64 off) { ft_inc_u32(pa, off); }
static void ft_dec_u16(u64 pa, u64 off) { ft_dec_u32(pa, off); }
@@FUNCTIONS@@
static void expect(unsigned index, u32 ro, u32 wx, u32 secondary) {
    assert(refs[index][2] == ro);
    assert(refs[index][3] == wx);
    assert(refs[index][1] == secondary);
}
static void run(bool_t four_k) {
    memset(refs, 0, sizeof(refs));
    memset(types, 11, sizeof(types));
    u64 pa = BASE + (four_k ? 0x1000 : 0);
    u64 rw = pa | 3 | ARM_PTE_PNX | ARM_PTE_NX;
    u64 ro = rw | ARM_PTE_AP_RO;
    u64 rx = ro & ~ARM_PTE_NX;
    u64 dbm = ro | ARM_PTE_WRITEABLE;
    update_refcounts_for_pte_change_geom(0, rw, four_k);
    expect(0, 0, 1, 0);
    update_refcounts_for_pte_change_geom(rw, ro, four_k);
    expect(0, 1, 0, 0);
    update_refcounts_for_pte_change_geom(ro, rx, four_k);
    expect(0, 0, 1, 0);
    update_refcounts_for_pte_change_geom(rx, dbm, four_k);
    expect(0, 0, 1, 0);
    update_refcounts_for_pte_change_geom(dbm, ro, four_k);
    expect(0, 1, 0, 0);
    update_refcounts_for_pte_change_geom(ro, ro | (1UL << 10), four_k);
    expect(0, 1, 0, 0);
    update_refcounts_for_pte_change_geom(ro | (1UL << 10), 0, four_k);
    expect(0, 0, 0, 0);
    /* Independent RO and WX aliases on the same physical frame. */
    update_refcounts_for_pte_change_geom(0, ro, four_k);
    update_refcounts_for_pte_change_geom(0, rw, four_k);
    update_refcounts_for_pte_change_geom(rw, ro, four_k);
    expect(0, 2, 0, 0);
    update_refcounts_for_pte_change_geom(ro, ro + 0x4000, four_k);
    expect(0, 1, 0, 0); expect(1, 1, 0, 0);
    update_refcounts_for_pte_change_geom(ro, 0, four_k);
    update_refcounts_for_pte_change_geom(ro + 0x4000, 0, four_k);
    expect(0, 0, 0, 0); expect(1, 0, 0, 0);
    /* Secondary page-table references use one permission-independent count. */
    types[0] = XNU_PAGE_TABLE_ALT;
    update_refcounts_for_pte_change_geom(0, rw, four_k);
    update_refcounts_for_pte_change_geom(rw, ro, four_k);
    expect(0, 0, 0, 1);
    update_refcounts_for_pte_change_geom(ro, 0, four_k);
    expect(0, 0, 0, 0);
    types[0] = 99;
    update_refcounts_for_pte_change_geom(0, rw, four_k);
    update_refcounts_for_pte_change_geom(rw, ro, four_k);
    update_refcounts_for_pte_change_geom(ro, 0, four_k);
    expect(0, 0, 0, 0);
    types[0] = 11;
    update_refcounts_for_pte_change_geom(pa | 2, pa | 1, four_k);
    expect(0, 0, 0, 0);
}
int main(void) {
    run(false); run(true);
    puts("PTE accounting: 16K/4K permissions, aliases, replacement, unmap, secondary/table and invalid entries pass");
}
'''.replace('@@MACROS@@', macros).replace('@@FUNCTIONS@@', functions)
with tempfile.TemporaryDirectory(prefix='m5-pte-refcounts-') as directory:
    path = Path(directory)
    (path / 'test.c').write_text(harness)
    subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-Werror',
                    str(path / 'test.c'), '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)

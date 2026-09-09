"""Run the production MTE batch implementation against bounded RAM mocks."""
from pathlib import Path
import ast
import subprocess
import tempfile
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parent
seed_path = ROOT / "m1n1/proxyclient/tools/sptm/seed.py"
tree = ast.parse(seed_path.read_text())
seed_function = next(n for n in tree.body
                     if isinstance(n, ast.FunctionDef) and n.name == "seed_sptm_bootstrap")
geometry = next(n for n in seed_function.body
                if isinstance(n, ast.If) and ast.unparse(n.test) == "m5_mte")
chosen = SimpleNamespace(dram_base=0x10000000000, dram_size=0x400000000)
mock_hv = SimpleNamespace(u=SimpleNamespace(
    adt={"/chosen": chosen}, mrs=lambda _: 0x80000103BC600000))
namespace = dict(hv=mock_hv, m5_mte=True, managed_start=0x10017824000,
                 managed_end=0x103DC600000, PAGE_SIZE=0x4000)
geometry_code = compile(ast.Module(body=[geometry], type_ignores=[]), str(seed_path), "exec")
exec(geometry_code, namespace)
assert (namespace['tag_base'], namespace['tag_end'], namespace['tag_count']) == (
    0x103BC600000, 0x103DC600000, 32768)
for invalid in (0x103BC600000, 0x80000103BC700000):
    mock_hv.u.mrs = lambda _, value=invalid: value
    try:
        exec(geometry_code, namespace)
    except RuntimeError:
        pass
    else:
        raise AssertionError('Unsafe inherited tag geometry was accepted')
tcr_call = next(n for n in ast.walk(seed_function)
                if isinstance(n, ast.Call) and ast.unparse(n.func) == 'hv.u.msr'
                and n.args and ast.unparse(n.args[0]) == 'TCR_EL12')
for is_m5, expected in ((True, 0x310800736511A511), (False, 0x10800336511A511)):
    value = eval(compile(ast.Expression(tcr_call.args[1]), str(seed_path), 'eval'),
                 dict(m5_mte=is_m5, TCR_BOOT=0x10800336511A511))
    assert value == expected
print('MTE production seed: hardware geometry, invalid-state rejection and M5-only TBI1/MTX0/MTX1 PASS')
base_assign = next(n for n in seed_function.body
                   if isinstance(n, ast.Assign) and any(
                       isinstance(t, ast.Name) and t.id == 'physmap_base' for t in n.targets))
for is_m5 in (False, True):
    value = eval(compile(ast.Expression(base_assign.value), str(seed_path), 'eval'),
                 dict(m5_mte=is_m5, PHYSMAP_BASE=0xFFFFFE4000000000,
                      M5_PHYSMAP_BASE=0xFFFFFE1000000000))
    if is_m5:
        for offset in (0, 0x3C749D2C0, 0x3FFFFFFFF):
            address = (value + offset) & ~63
            packed = (address >> 6) & 0xFFFFFFFF
            assert not packed & 0x80000000
            assert (0xFFFFFE0000000000 | (packed << 6)) == address
    else:
        assert value == 0xFFFFFE4000000000
assert (0xFFFFFE0000000000 | (((0xFFFFFE43C749D2C0 >> 6) & 0xFFFFFFFF) << 6)) == 0xFFFFFE03C749D2C0
print('M5 compact VM-pointer round trips and guest09 failure reproduction PASS')
source = (ROOT / "m1n1/src/sptm/xnu.c").read_text()
start = source.index("struct sptm_mte_state {")
end = source.index("__attribute__((used, retain)) u64 sptm_emul_seed_alt_ptd_pool")
production = source[start:end].replace("__attribute__((used, retain))", "")
prefix = r"""
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
typedef uint64_t u64;
typedef uint32_t u32;
typedef uint16_t u16;
typedef uint8_t u8;
typedef int bool_t;
#define PAGE_SIZE_GUEST_EMUL 0x4000UL
#define ARM_PTE_PAGE_MASK 0x0000ffffffffc000UL
#define ARM_PTE_ATTRINDX_MASK (7UL << 2)
static u64 chip_id = 0x8142;
static struct { u64 frame_table_size, vm_first_phys, frame_table_base;
                u64 fast_path_calls; } g_state;
static u64 ptes[128], invalidations;
static bool_t ft_get_idx(u64 pa, u64 *idx) {
    if (pa < g_state.vm_first_phys) return 0;
    *idx = (pa - g_state.vm_first_phys) / 0x4000;
    return *idx < 128;
}
static u8 *frame(u64 pa) { u64 i; assert(ft_get_idx(pa,&i));
    return (u8 *)g_state.frame_table_base + i*16; }
static u8 ft_get_type(u64 pa) { return frame(pa)[2]; }
static u32 ft_read_u32(u64 pa,u64 off) { return *(u32 *)(frame(pa)+off); }
static u32 ft_read_page_refcount(u64 pa) {
    return ft_read_u32(pa,8)+ft_read_u32(pa,12); }
static bool_t papt_va_for_managed_pa(u64 pa,u64 *va) { *va=pa; return 1; }
static u64 ttbr1_el12_root_pa(void) { return 1; }
static u64 walk_to_l3_16k(u64 root,u64 va,u64 *idx) {
    (void)root; return ft_get_idx(va,idx); }
static volatile u64 *pt_slot_ptr(u64 l3,u64 idx) { (void)l3; return &ptes[idx]; }
static u64 pt_read_slot_poc(volatile u64 *p) { return *p; }
static void clean_pt_slot_poc(volatile u64 *p) { (void)p; }
static void ft_clean_entry_idx(u64 i) { (void)i; }
static void tlbi_vmalle1is(void) { invalidations++; }
static void sev(void) {}
"""
tests = r"""
int main(void) {
    void *ram; assert(!posix_memalign(&ram,0x4000,128*0x4000));
    u8 frames[128*16] = {0};
    u64 base=(u64)ram, storage=base+124*0x4000;
    g_state.vm_first_phys=base; g_state.frame_table_size=sizeof(frames);
    g_state.frame_table_base=(u64)frames;
    for (u64 i=0;i<128;i++) {
        frames[i*16+2]=i<124 ? 11 : 32;
        ptes[i]=(base+i*0x4000)|3;
    }
    assert(sptm_emul_seed_mte(base,storage,4*0x4000));
    u64 *list=ram, regs[8]={base,2,0};
    list[0]=base+0x4000; list[1]=base+0x8000;
    assert(xnu_tag_papt_multipage(regs,1));
    assert(regs[0]==0 && (frames[16+4]&4) && (frames[32+4]&4));
    assert(ft_read_u32(storage,4)==2 && (ptes[1]&28)==16 && invalidations==1);
    regs[0]=base; assert(!xnu_tag_papt_multipage(regs,1));
    assert(ft_read_u32(storage,4)==2 && invalidations==1);
    regs[0]=base; assert(xnu_tag_papt_multipage(regs,0));
    assert(!ft_read_u32(storage,4) && !(frames[20]&4) && !(ptes[1]&28));
    assert(g_sptm_mte.tagged_pages==0);
    /* Invalid second frame must leave the first frame and PTE untouched. */
    list[1]=base+0x8001; regs[0]=base;
    assert(!xnu_tag_papt_multipage(regs,1));
    assert(!(frames[20]&4) && !(ptes[1]&28) && !ft_read_u32(storage,4));
    list[1]=list[0]; assert(!xnu_tag_papt_multipage(regs,1));
    list[1]=base+0x8000; *(u32 *)(frames+32+8)=1;
    assert(!xnu_tag_papt_multipage(regs,1)); *(u32 *)(frames+32+8)=0;
    /* Deferred invalidation return code and shared storage-page counts. */
    regs[2]=0x100; assert(xnu_tag_papt_multipage(regs,1)); assert(regs[0]==5);
    regs[0]=base; assert(xnu_tag_papt_multipage(regs,0));
    regs[0]=base; regs[2]=1; assert(!xnu_tag_papt_multipage(regs,1));
    regs[2]=0; regs[1]=65; assert(!xnu_tag_papt_multipage(regs,1));
    regs[1]=2; frames[16+2]=32; assert(!xnu_tag_papt_multipage(regs,1));
    chip_id=0x8140; assert(!sptm_emul_seed_mte(base,storage,4*0x4000));
    puts("MTE production C: tag/untag, shared counts, atomic rejection, bounds, options and SoC guard PASS");
    free(ram);
}
"""
with tempfile.TemporaryDirectory(prefix="j813-mte-test-") as tmp:
    path = Path(tmp) / "test.c"
    path.write_text(prefix + production + tests)
    binary = Path(tmp) / "test"
    subprocess.run(["clang", "-std=c11", "-D_POSIX_C_SOURCE=200112L",
                    "-Wall", "-Wextra", "-Werror", str(path), "-o", str(binary)], check=True)
    subprocess.run([str(binary)], check=True)

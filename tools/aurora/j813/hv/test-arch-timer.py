"""Exercise production architectural timer helpers with register mocks."""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent
source = (ROOT / 'm1n1/src/hv_exc.c').read_text()


def function(name):
    match = re.search(r'^static (?:u64|void|bool) ' + name + r'\([^;]+?\)\n\{',
                      source, re.M)
    if not match:
        raise RuntimeError(f'Production helper missing: {name}')
    depth = 1
    pos = match.end()
    while depth:
        depth += (source[pos] == '{') - (source[pos] == '}')
        pos += 1
    return source[match.start():pos]


helpers = '\n'.join(function(name) for name in (
    'hv_arch_timer_read_ctl', 'hv_arch_timer_write_ctl',
    'hv_vm_timer_gate', 'hv_arch_timer_firing', 'hv_update_fiq'))
mock = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
typedef uint64_t u64;
#define T8142 0x8142
#define CNTx_CTL_ENABLE 1
#define CNTx_CTL_IMASK 2
#define CNTx_CTL_ISTATUS 4
#define VM_TMR_FIQ_ENA_ENA_P 1
#define VM_TMR_FIQ_ENA_ENA_V 2
#define HCR_VF 64
enum { SYS_CNTP_CTL_EL02, SYS_CNTV_CTL_EL02,
       SYS_IMP_APL_VM_TMR_FIQ_ENA_EL2, HCR_EL2 };
static u64 registers[2][4];
static unsigned chip_id = T8142, cpu;
static struct {
    u64 arch_timer_ctl[2];
    bool arch_timer_valid[2];
    bool ipi_pending, pmc_pending;
} pcpu[2];
#define PERCPU(field) pcpu[cpu].field
#define mrs(reg) registers[cpu][reg]
static void msr(unsigned reg, u64 val) {
    if (reg <= SYS_CNTV_CTL_EL02)
        val = (val & 3) | (registers[cpu][reg] & 4);
    registers[cpu][reg] = val;
}
#define reg_set(reg, bits) msr(reg, mrs(reg) | (bits))
#define reg_clr(reg, bits) msr(reg, mrs(reg) & ~(bits))
#define sysop(op) ((void)0)
static bool hv_kernel_cntv_pending(void) { return false; }
static void hv_write_hcr(u64 value) { msr(HCR_EL2, value); }
'''
checks = r'''
int main(void) {
    /* The measured guest13 state must not inject FIQ. */
    registers[0][SYS_CNTP_CTL_EL02] = 4;
    registers[0][SYS_CNTV_CTL_EL02] = 7;
    hv_update_fiq();
    assert(!(mrs(HCR_EL2) & HCR_VF));
    assert(mrs(SYS_CNTV_CTL_EL02) == 7);
    assert(hv_arch_timer_read_ctl(true) == 7);

    /* An actual guest deadline stays pending through the host mask. */
    hv_arch_timer_write_ctl(true, 1);
    hv_update_fiq();
    assert(mrs(HCR_EL2) & HCR_VF);
    assert(mrs(SYS_CNTV_CTL_EL02) == 7);
    assert(hv_arch_timer_read_ctl(true) == 5);
    hv_update_fiq();
    assert(mrs(HCR_EL2) & HCR_VF);

    /* Reprogramming the compare clears status and releases the host mask. */
    registers[0][SYS_CNTV_CTL_EL02] &= ~4;
    hv_update_fiq();
    assert(!(mrs(HCR_EL2) & HCR_VF));
    assert(mrs(SYS_CNTV_CTL_EL02) == 1);
    registers[0][SYS_CNTV_CTL_EL02] |= 4;
    hv_arch_timer_write_ctl(true, 3);
    hv_update_fiq();
    assert(!(mrs(HCR_EL2) & HCR_VF));
    assert(hv_arch_timer_read_ctl(true) == 7);
    hv_arch_timer_write_ctl(true, 0);
    hv_update_fiq();
    assert(!(mrs(HCR_EL2) & HCR_VF));

    /* Physical timer and per-CPU masks are independent. */
    hv_arch_timer_write_ctl(false, 1);
    hv_update_fiq();
    assert(mrs(HCR_EL2) & HCR_VF);
    assert(hv_arch_timer_read_ctl(false) == 5);
    cpu = 1;
    registers[1][SYS_CNTP_CTL_EL02] = 7;
    hv_update_fiq();
    assert(!(mrs(HCR_EL2) & HCR_VF));
    assert(hv_arch_timer_read_ctl(false) == 7);
    cpu = 0;
    assert(hv_arch_timer_read_ctl(false) == 5);

    /* Other SoCs keep native CTL and Apple gate semantics. */
    chip_id = 0x8140;
    registers[0][SYS_CNTP_CTL_EL02] = 7;
    registers[0][SYS_CNTV_CTL_EL02] = 5;
    hv_update_fiq();
    assert(mrs(HCR_EL2) & HCR_VF);
    assert(mrs(SYS_IMP_APL_VM_TMR_FIQ_ENA_EL2) == 1);
    assert(hv_arch_timer_read_ctl(false) == 7);
    assert(hv_arch_timer_read_ctl(true) == 5);
    hv_arch_timer_write_ctl(true, 3);
    hv_update_fiq();
    assert(!(mrs(HCR_EL2) & HCR_VF));
    assert(mrs(SYS_IMP_APL_VM_TMR_FIQ_ENA_EL2) == 3);
    puts("Architectural timer masks: guest13 regression, deadlines, CPUs, and non-M5 passed");
}
'''
with tempfile.TemporaryDirectory() as temp:
    path = Path(temp)
    (path / 'test.c').write_text(mock + helpers + checks)
    subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-Werror',
                    str(path / 'test.c'), '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)

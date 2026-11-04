#!/usr/bin/env python3
"""Exercise the actual recovery entry with a live scratch mapping."""
import pathlib
import subprocess
import tempfile

source = pathlib.Path(__file__).with_name('ane_tm.c').read_text()
recovery = source[source.index('int ane_tm_recover('):]
prefix = r'''
#include <stdbool.h>
#include <stdint.h>
#include <errno.h>
#include <stdio.h>
typedef uint32_t u32;
struct ane_device { int engine_lock, wedged, scratch_pages; bool tm_retention; int dev; char *engine; u32 tm_status_fresh; };
static int cycles, module_puts, reclaims;
#define THIS_MODULE 0
#define ANE_TM_BASE 0
#define TM_STATUS 0
#define TM_IS_IDLE 1
#define lockdep_assert_held(x) ((void)0)
#define dev_err(...) ((void)0)
#define dev_info(...) ((void)0)
#define atomic_read(p) (*(p))
#define atomic_xchg(p, v) ({ int old=*(p); *(p)=(v); old; })
#define module_put(x) (module_puts++)
#define readl_poll_timeout(addr, status, cond, delay, timeout) ((status)=1, 0)
static int ane_pd_cycle(struct ane_device *a) { (void)a; cycles++; return 0; }
static int ane_ps_verify_on(struct ane_device *a) { (void)a; return 0; }
static void ane_tm_drain_retained(struct ane_device *a) { (void)a; }
static void ane_tm_enable(struct ane_device *a, bool b) { (void)a; (void)b; }
static u32 ane_rec_read32(struct ane_device *a, const char *s, void *p) { (void)a; (void)s; (void)p; return 1; }
static void ane_reclaim_preserved(struct ane_device *a) { (void)a; reclaims++; }
'''
main = r'''
int main(void) {
    struct ane_device a={.wedged=1, .scratch_pages=1};
    int rc=ane_tm_recover(&a);
    if (rc != -EBUSY || cycles || module_puts || reclaims || !a.wedged) {
        fprintf(stderr, "retained scratch: rc=%d cycles=%d puts=%d reclaims=%d wedged=%d\n", rc, cycles, module_puts, reclaims, a.wedged); return 1;
    }
    a.scratch_pages=0;
    rc=ane_tm_recover(&a);
    if (rc || cycles != 1 || module_puts != 1 || reclaims != 1 || a.wedged) {
        fprintf(stderr, "scratch-free recovery failed: rc=%d cycles=%d puts=%d reclaims=%d wedged=%d\n",
                rc, cycles, module_puts, reclaims, a.wedged); return 1;
    }
    printf("retained scratch blocks recovery: PASS\n"); return 0;
}
'''
with tempfile.TemporaryDirectory() as tmp:
    binary = pathlib.Path(tmp) / 'ane-recovery-test'
    subprocess.run(['cc', '-std=gnu11', '-x', 'c', '-o', str(binary), '-'],
                   input=prefix + recovery + main, text=True, check=True)
    subprocess.run([str(binary)], check=True)

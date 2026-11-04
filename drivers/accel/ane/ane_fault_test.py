#!/usr/bin/env python3
"""Run the task-manager fault paths with simulated device state."""
import pathlib
import subprocess
import tempfile

source = pathlib.Path(__file__).with_name('ane_tm.c').read_text()
functions = source[source.index('static int ane_tm_poll_step('):source.index('\n/*\n * Bounded recovery')]
prefix = r'''
#include <stdbool.h>
#include <stdint.h>
#include <errno.h>
#include <stdio.h>
typedef uint32_t u32;
typedef uint64_t u64;
struct ane_dart_scratch { int unused; };
struct ane_device { int engine_lock, wedged, tm_retention, dev; struct ane_dart_scratch scratch[4]; int scratch_pages; };
struct ane_request { int qid, nid, td_count; };
#define ANE_DART_SCRATCH_MAX 4
#define THIS_MODULE 0
#define TQ_NID1(qid) 0
#define TQ_STATUS(qid) 1
#define lockdep_assert_held(x) ((void)0)
#define wmb() ((void)0)
#define rmb() ((void)0)
#define dev_err(...) ((void)0)
#define __module_get(x) ((void)0)
#define atomic_xchg(ptr, value) ({ int old = *(ptr); *(ptr) = (value); old; })
#define read_poll_timeout(op, status, cond, delay, timeout, sleep, ...) ((status) = op(__VA_ARGS__), 0)
static int reads, scenario, drains, released, recovery, recovery_rc;
static bool ane_dart_faulted(struct ane_device *a, u64 *i, u32 *s) {
    int n = reads++; (void)a; (void)i; (void)s;
    if (scenario == 1) return n == 0;
    if (scenario == 2) return n == 0 || n == 1 || n == 3;
    if (scenario == 3) return n < 2;
    if (scenario == 4) return n < 3;
    if (scenario == 5) return true;
    return false;
}
static int ane_tm_collect_events(struct ane_device *a, struct ane_request *r, u32 *finished) {
    (void)a; if (!r) return 0; *finished = 3; return 1;
}
static void ane_dart_mask(struct ane_device *a) { (void)a; }
static void ane_dart_unmask(struct ane_device *a) { (void)a; }
static void ane_tm_push_tq(struct ane_device *a, struct ane_request *r) { (void)a; (void)r; }
static int ane_dart_drain_fault(struct ane_device *a, u64 i, struct ane_dart_scratch *s, int *p) {
    (void)a; (void)i; (void)s; drains++;
    if (scenario == 2 && drains == 2) return -ENOMEM;
    (*p)++; return 0;
}
static void ane_dart_release_scratch(struct ane_device *a, struct ane_dart_scratch *s, int n) {
    (void)a; (void)s; released += n;
}
static int ane_tm_recover(struct ane_device *a) { (void)a; recovery++; return recovery_rc; }
static u32 tq_read32(struct ane_device *a, int r) { (void)a; (void)r; return 4 << 8; }
static void tq_write32(struct ane_device *a, int r, u32 v) { (void)a; (void)r; (void)v; }
'''
main = r'''
int main(void) {
    struct ane_device a = {0};
    struct ane_request r = {.qid=4, .nid=4};
    int rc = ane_tm_execute(&a, &r);
    if (rc != 0 || recovery) { fprintf(stderr, "normal: %d\n", rc); return 1; }
    scenario=1; reads=recovery=0;
    rc = ane_tm_execute(&a, &r);
    if (rc != -EIO || recovery != 1) {
        fprintf(stderr, "transient fault: rc=%d recovery=%d\n", rc, recovery); return 1;
    }
    a.wedged=0; a.scratch_pages=0; scenario=2; reads=drains=recovery=released=0; recovery_rc=-EIO;
    rc = ane_tm_execute(&a, &r);
    if (rc != -ENOMEM || drains != 2 || released || !a.wedged || a.scratch_pages != 1) {
        fprintf(stderr, "failed second drain: rc=%d drains=%d released=%d wedged=%d pages=%d\n", rc, drains, released, a.wedged, a.scratch_pages); return 1;
    }
    a.wedged=0; a.scratch_pages=0; scenario=3; reads=drains=recovery=released=0;
    rc = ane_tm_execute(&a, &r);
    if (rc != -EIO || drains != 1 || released != 1 || a.wedged || recovery || a.scratch_pages) {
        fprintf(stderr, "contained drain: rc=%d drains=%d released=%d wedged=%d pages=%d\n",
                rc, drains, released, a.wedged, a.scratch_pages); return 1;
    }
    a.wedged=0; a.scratch_pages=0; scenario=4; reads=drains=recovery=released=0;
    rc = ane_tm_execute(&a, &r);
    if (rc != -EIO || drains != 1 || released || !a.wedged || a.scratch_pages != 1) {
        fprintf(stderr, "fault after drain: rc=%d drains=%d released=%d wedged=%d pages=%d\n",
                rc, drains, released, a.wedged, a.scratch_pages); return 1;
    }
    a.wedged=0; a.scratch_pages=0; scenario=5; reads=drains=recovery=released=0;
    rc = ane_tm_execute(&a, &r);
    if (rc != -EIO || drains != ANE_DART_SCRATCH_MAX || released || !a.wedged ||
        a.scratch_pages != ANE_DART_SCRATCH_MAX) {
        fprintf(stderr, "scratch cap: rc=%d drains=%d released=%d wedged=%d pages=%d\n",
                rc, drains, released, a.wedged, a.scratch_pages); return 1;
    }
    puts("task manager fault paths: PASS"); return 0;
}
'''
with tempfile.TemporaryDirectory() as tmp:
    binary = pathlib.Path(tmp) / 'ane-fault-test'
    subprocess.run(['cc', '-std=gnu11', '-x', 'c', '-o', str(binary), '-'],
                   input=prefix + functions + main, text=True, check=True)
    subprocess.run([str(binary)], check=True)

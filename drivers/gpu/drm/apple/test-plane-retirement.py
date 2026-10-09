#!/usr/bin/env python3
"""Execute the driver's plane preparation/retirement with allocation fault injection."""
from pathlib import Path
import os
import shlex
import subprocess
import tempfile

root = Path(__file__).resolve().parent
plane = (root / 'plane.c').read_text()
swap = (root / 'iomfb_template.c').read_text()
driver = (root / 'apple_drv.c').read_text()

def block(source, marker):
    start = source.index(marker)
    end = source.index('{', start) + 1
    depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

prepare = block(plane, 'static int apple_plane_prepare_fb(')
cleanup = block(plane, 'static void apple_plane_cleanup_fb(')
reset = block(plane, 'static void apple_plane_reset(')
duplicate = block(plane, 'static struct drm_plane_state *\napple_plane_duplicate_state(')
destroy = block(plane, 'static void apple_plane_destroy_state(')
retire = block(swap, 'if (old_state->crtc == crtc && old_state->fb &&')
tail = block(driver, 'static void apple_atomic_commit_tail(')

stub = r'''
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#define GFP_KERNEL 0
#define ENOMEM 12
struct drm_framebuffer { int refs; };
struct drm_crtc { int unused; };
struct drm_atomic_state { int unused; };
struct drm_plane_state {
    struct drm_atomic_state *state;
    struct drm_crtc *crtc;
    struct drm_framebuffer *fb;
};
struct drm_plane { struct drm_plane_state *state; };
struct list_head { int unused; };
struct dcp_fb_reference {
    struct list_head head;
    struct drm_framebuffer *fb;
    unsigned swap_id;
    bool armed;
};
struct apple_plane_state {
    struct drm_plane_state base;
    int surf, src_rect, dst_rect;
    unsigned long long iova;
    struct dcp_fb_reference *retiring_fb;
};
#define container_of(ptr, type, member) ((type *)((char *)(ptr) - offsetof(type, member)))
#define to_apple_plane_state(s) container_of(s, struct apple_plane_state, base)
static int fail_alloc, allocs, frees, fence_error, fence_calls;
static void *kzalloc(size_t n, int flags) {
    (void)flags;
    if (fail_alloc) return NULL;
    allocs++;
    return calloc(1, n);
}
#define kzalloc_obj(obj) kzalloc(sizeof(obj), GFP_KERNEL)
static void kfree(void *p) { if (p) { frees++; free(p); } }
static struct drm_plane_state *old;
static struct drm_plane_state *drm_atomic_get_old_plane_state(
    struct drm_atomic_state *s, struct drm_plane *p) {
    (void)s; (void)p; return old;
}
static int drm_gem_plane_helper_prepare_fb(struct drm_plane *p, struct drm_plane_state *s) {
    (void)p; (void)s; fence_calls++; return fence_error;
}
static void __drm_atomic_helper_plane_duplicate_state(struct drm_plane *p,
                                                     struct drm_plane_state *s) { *s = *p->state; }
static void __drm_atomic_helper_plane_destroy_state(struct drm_plane_state *s) { (void)s; }
static void __drm_atomic_helper_plane_reset(struct drm_plane *p,
                                            struct drm_plane_state *s) { p->state = s; }
static void drm_atomic_helper_plane_destroy_state(struct drm_plane *p,
                                                 struct drm_plane_state *s) { (void)p; kfree(s); }
static void drm_framebuffer_get(struct drm_framebuffer *fb) { fb->refs++; }
struct dcp { int swapped_out_lock; struct list_head swapped_out_fbs; };
static struct dcp_fb_reference *queued;
#define spin_lock_irqsave(lock, flags) ((void)(lock), (flags) = 0)
#define spin_unlock_irqrestore(lock, flags) ((void)(lock), (void)(flags))
static void list_add_tail(struct list_head *head, struct list_head *list) {
    (void)list;
    assert(!queued);
    queued = (struct dcp_fb_reference *)head;
}
'''
code = stub + prepare + '\n' + cleanup + '\n' + reset + '\n' + duplicate + '\n' + destroy + r'''
static void retire(struct dcp *dcp, struct drm_crtc *crtc,
                   struct drm_plane_state *old_state, struct drm_plane_state *new_state) {
    struct apple_plane_state *apple_state = to_apple_plane_state(new_state);
    struct dcp_fb_reference *entry;
    unsigned long flags;
''' + retire.replace('continue;', 'return;') + r'''
}
struct drm_crtc_state { bool active; };
struct platform_device { struct dcp *d; };
struct apple_crtc { struct platform_device *dcp; };
static struct apple_crtc tail_crtc;
static struct drm_crtc *tail_base;
static struct drm_crtc_state inactive_state;
static struct drm_plane_state *tail_plane;
#define to_apple_crtc(c) ((void)(c), &tail_crtc)
#define for_each_old_crtc_in_state(s,c,o,i) \
    for ((void)(s), (i)=0; (i)<1 && ((c)=tail_base, (o)=&inactive_state, true); (i)++)
static void dcp_retain_unbound_fbs_pdev(struct platform_device *pdev,
                                      struct drm_crtc *crtc, struct drm_atomic_state *state) {
    (void)state;
    retire(pdev->d, crtc, old, tail_plane);
}
static void drm_atomic_helper_commit_tail_rpm(struct drm_atomic_state *state) {
    (void)state;
    assert(!inactive_state.active); /* ACTIVE_ONLY would skip plane hooks. */
    assert(queued && queued->fb==old->fb); /* held before standard cleanup */
}
''' + tail + r'''
int main(void) {
    struct drm_framebuffer a = {1}, b = {1};
    struct drm_crtc c, other;
	struct drm_plane fresh = {0};
	apple_plane_reset(&fresh);
	assert(fresh.state);
	struct apple_plane_state *fresh_state = to_apple_plane_state(fresh.state);
	fresh_state->retiring_fb = kzalloc(sizeof(*fresh_state->retiring_fb), GFP_KERNEL);
	assert(fresh_state->retiring_fb);
	int reset_frees = frees;
	apple_plane_reset(&fresh);
	assert(fresh.state && frees == reset_frees + 2);
	apple_plane_destroy_state(&fresh, fresh.state);
	fresh.state = NULL;
    struct apple_plane_state prior = {.base = {.fb = &a, .crtc = &c},
                                      .surf = 7, .src_rect = 8, .dst_rect = 9, .iova = 0x12345000};
    struct drm_plane p = {.state = &prior.base};
    struct dcp d = {0};
    old = &prior.base;
    struct drm_plane_state *next = apple_plane_duplicate_state(&p);
    struct apple_plane_state *n = to_apple_plane_state(next);
    assert(n->surf == 7 && n->src_rect == 8 && n->dst_rect == 9 && n->iova == prior.iova);
    assert(!n->retiring_fb);
    int before = allocs;
    assert(apple_plane_prepare_fb(&p, next) == 0 && allocs == before);
    next->fb = &b;
    fence_error = -5;
    assert(apple_plane_prepare_fb(&p, next) == -5 && allocs == before);
    fence_error = 0;
    fail_alloc = 1;
    assert(apple_plane_prepare_fb(&p, next) == -ENOMEM);
    assert(!n->retiring_fb && a.refs == 1);
    fail_alloc = 0;
    assert(apple_plane_prepare_fb(&p, next) == 0 && n->retiring_fb);
    before = allocs;
    assert(apple_plane_prepare_fb(&p, next) == 0 && allocs == before);
    retire(&d, &other, old, next);
    assert(!queued && a.refs == 1);
    retire(&d, &c, old, next);
    assert(queued && queued->fb == &a && a.refs == 2 && !n->retiring_fb);
    assert(!queued->armed && allocs == before);
    retire(&d, &c, old, next); /* disable plus flush must consume only once */
    assert(a.refs == 2 && allocs == before);
    apple_plane_destroy_state(&p, next);
    assert(a.refs == 2); /* DCP still owns the reference after atomic cleanup. */
    queued->fb->refs--;
    kfree(queued);
    queued = NULL;
    /* Disable preparation owns only bookkeeping until the commit actually runs. */
    next = apple_plane_duplicate_state(&p);
    next->fb = NULL;
    assert(apple_plane_prepare_fb(&p, next) == 0);
    assert(to_apple_plane_state(next)->retiring_fb);
    apple_plane_destroy_state(&p, next); /* rejected/cancelled commit */
    assert(a.refs == 1 && allocs == frees && fence_calls == 6);
    /* Moving the same framebuffer to another CRTC still retires the old pipe. */
    next = apple_plane_duplicate_state(&p);
    next->crtc = &other;
    fail_alloc = 1;
    assert(apple_plane_prepare_fb(&p, next) == -ENOMEM);
    fail_alloc = 0;
    assert(apple_plane_prepare_fb(&p, next) == 0);
    retire(&d, &c, old, next);
    assert(queued && queued->fb == &a && a.refs == 2);
    apple_plane_destroy_state(&p, next);
    queued->fb->refs--;
    kfree(queued);
    queued = NULL;
    /* prepare/unprepare can be retried on an atomic state without leaking. */
    next = apple_plane_duplicate_state(&p);
    next->fb = NULL;
    assert(apple_plane_prepare_fb(&p, next) == 0);
    apple_plane_cleanup_fb(&p, next);
    assert(!to_apple_plane_state(next)->retiring_fb);
    assert(apple_plane_prepare_fb(&p, next) == 0);
    apple_plane_destroy_state(&p, next);
    assert(a.refs == 1 && allocs == frees);
    /* Execute the real commit-tail wrapper with an already inactive CRTC. */
    next = apple_plane_duplicate_state(&p);
    next->fb = NULL;
    assert(apple_plane_prepare_fb(&p, next) == 0);
    struct platform_device pdev = {.d = &d};
    struct drm_atomic_state atomic = {0};
    tail_crtc.dcp = &pdev;
    tail_base = &c;
    tail_plane = next;
    apple_atomic_commit_tail(&atomic);
    assert(queued && a.refs == 2);
    apple_plane_destroy_state(&p, next);
    queued->fb->refs--;
    kfree(queued);
    assert(a.refs == 1 && allocs == frees);
    return 0;
}
'''
with tempfile.TemporaryDirectory() as tmp:
    source = Path(tmp) / 'test.c'
    binary = Path(tmp) / 'test'
    source.write_text(code)
    subprocess.run([os.environ.get('CC', 'cc'), '-std=gnu11', '-Wall', '-Wextra', '-Werror',
                    '-Wno-unused-parameter',
                    *shlex.split(os.environ.get('CFLAGS', '')),
                    str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('PASS actual DCP plane functions: allocation failures, fences, cancellation, ownership transfer, state duplication')

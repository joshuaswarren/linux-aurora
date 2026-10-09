#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only OR MIT
"""Execute DCP retirement functions across rejection, replay and ID wrap."""
from pathlib import Path
import os
import subprocess
import tempfile

source = Path(__file__).with_name('iomfb_template.c').read_text()
def block(marker):
    start = source.index(marker)
    end = source.index('{', start) + 1
    depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

functions = '\n'.join(block('static void ' + name + '(') for name in (
    'dcp_arm_swapped_out_fbs', 'dcp_unarm_swapped_out_fbs',
    'dcp_release_swapped_out_fbs'))
stub = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
typedef uint32_t u32;
typedef int32_t s32;
struct list_head { struct list_head *next, *prev; };
#define LIST_HEAD(name) struct list_head name = { &name, &name }
#define container_of(p,t,m) ((t *)((char *)(p) - offsetof(t,m)))
#define list_first_entry(h,t,m) container_of((h)->next,t,m)
#define list_for_each_entry(p,h,m) \
    for (p=list_first_entry(h,__typeof__(*p),m); &p->m!=(h); \
         p=list_first_entry(&p->m,__typeof__(*p),m))
#define list_for_each_entry_safe(p,n,h,m) \
    for (p=list_first_entry(h,__typeof__(*p),m), \
         n=list_first_entry(&p->m,__typeof__(*p),m); &p->m!=(h); \
         p=n,n=list_first_entry(&n->m,__typeof__(*n),m))
#define spin_lock_irqsave(l,f) ((void)(l),(f)=0)
#define spin_unlock_irqrestore(l,f) ((void)(l),(void)(f))
static int freed;
static void kfree(void *p) { free(p); freed++; }
static bool list_empty(struct list_head *h) { return h->next==h; }
static void list_del(struct list_head *p) { p->prev->next=p->next; p->next->prev=p->prev; }
static void list_add_tail(struct list_head *p,struct list_head *h) {
    p->next=h; p->prev=h->prev; h->prev->next=p; h->prev=p;
}
static void list_move_tail(struct list_head *p,struct list_head *h) { list_del(p); list_add_tail(p,h); }
struct drm_framebuffer { int refs; };
static void drm_framebuffer_put(struct drm_framebuffer *f) { assert(f->refs>0); f->refs--; }
struct dcp_fb_reference {
    struct list_head head;
    struct drm_framebuffer *fb;
    u32 swap_id;
    bool armed;
};
struct apple_dcp {
    struct list_head swapped_out_fbs;
    int swapped_out_lock;
    u32 last_swap_id;
    bool have_swap_complete;
};
static struct dcp_fb_reference *hold(struct apple_dcp *d,struct drm_framebuffer *f) {
    struct dcp_fb_reference *e=calloc(1,sizeof(*e));
    assert(e); e->fb=f; f->refs++; list_add_tail(&e->head,&d->swapped_out_fbs); return e;
}
'''
main = r'''
int main(void) {
    struct apple_dcp d={0};
    d.swapped_out_fbs.next=d.swapped_out_fbs.prev=&d.swapped_out_fbs;
    struct drm_framebuffer a={1},b={1};
    struct dcp_fb_reference *e=hold(&d,&a);
    dcp_arm_swapped_out_fbs(&d,10);
    assert(e->armed && e->swap_id==10);
    dcp_unarm_swapped_out_fbs(&d,9); /* only the rejected swap is unarmed */
    assert(e->armed);
    dcp_unarm_swapped_out_fbs(&d,10);
    assert(!e->armed);
    d.have_swap_complete=true;
    d.last_swap_id=11; /* brightness replay of the still-scanned old surface */
    dcp_release_swapped_out_fbs(&d);
    assert(a.refs==2 && freed==0);
    dcp_arm_swapped_out_fbs(&d,12); /* actual replacement */
    d.last_swap_id=12;
    dcp_release_swapped_out_fbs(&d);
    assert(a.refs==2); /* preserve the existing extra-completion grace period */
    d.last_swap_id=13;
    dcp_release_swapped_out_fbs(&d);
    assert(a.refs==1 && list_empty(&d.swapped_out_fbs));
    hold(&d,&a); hold(&d,&b);
    dcp_arm_swapped_out_fbs(&d,UINT32_MAX);
    d.have_swap_complete=false;
    d.last_swap_id=0;
    dcp_release_swapped_out_fbs(&d);
    assert(a.refs==2 && b.refs==2);
    d.have_swap_complete=true;
    dcp_release_swapped_out_fbs(&d);
    assert(a.refs==1 && b.refs==1 && freed==3);
    return 0;
}
'''
# Submission rejection must disarm before signalling a synthetic flip; the
# accepted-surface cache is published only after the rejection early return.
callback = block('static void dcp_swapped(')
assert callback.index('dcp_unarm_swapped_out_fbs') < callback.index('dcp_drm_crtc_vblank')
assert callback.index('return;') < callback.index('memcpy(&dcp->h17p_last_swap')
flush = block('void DCP_FW_NAME(iomfb_flush)(')
assert 'memcpy(&dcp->h17p_last_swap' not in flush
assert 'memcpy(&dcp->h17p_pending_swap' in flush
with tempfile.TemporaryDirectory() as tmp:
    c=Path(tmp)/'test.c'
    exe=Path(tmp)/'test'
    c.write_text(stub+functions+main)
    subprocess.run([os.environ.get('CC','cc'),'-std=gnu11','-Wall','-Wextra','-Werror',
                    str(c),'-o',str(exe)],check=True)
    subprocess.run([str(exe)],check=True)
print('PASS actual DCP retirement: rejection then brightness replay, replacement completion, ID wrap')

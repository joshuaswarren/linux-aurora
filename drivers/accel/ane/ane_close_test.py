#!/usr/bin/env python3
"""Exercise the DRM release and postclose functions with failed resume."""
import pathlib
import subprocess
import tempfile

source = pathlib.Path(__file__).with_name('ane_drv.c').read_text()
post = source[source.index('static void ane_drm_postclose('):source.index('\n/*\n * The driver only owns', source.index('static void ane_drm_postclose('))]
release = source[source.index('static int ane_drm_release('):source.index('\nstatic int ane_drm_mmap(', source.index('static int ane_drm_release('))]
prefix = r'''
#include <stdbool.h>
#include <stdio.h>
#include <errno.h>
struct device { int unused; };
struct ane_device { int engine_lock; bool removed; struct device *dev; };
struct drm_device { void *dev_private; struct device *dev; };
struct drm_minor { struct drm_device *dev; };
struct drm_file { struct drm_minor *minor; };
struct file { void *private_data; };
struct inode { int unused; };
static int get_count, put_count, releases, recoveries, resume_rc;
#define mutex_lock(x) ((void)0)
#define mutex_unlock(x) ((void)0)
static int pm_runtime_get_sync(struct device *d) { (void)d; get_count++; return resume_rc; }
static int pm_runtime_resume_and_get(struct device *d) { (void)d; get_count++; return resume_rc; }
static void pm_runtime_put_autosuspend(struct device *d) { (void)d; put_count++; }
static void pm_runtime_mark_last_busy(struct device *d) { (void)d; }
static struct device *get_device(struct device *d) { return d; }
static void put_device(struct device *d) { (void)d; }
static int ane_tm_recover(struct ane_device *a) { (void)a; recoveries++; return 0; }
static void ane_drm_postclose(struct drm_device *, struct drm_file *);
static int drm_release(struct inode *i, struct file *f) {
    struct drm_file *p = f->private_data; (void)i; releases++;
    ane_drm_postclose(p->minor->dev, p); return 0;
}
'''
main = r'''
int main(void) {
    struct device d={0}; struct ane_device a={.dev=&d};
    struct drm_device drm={.dev_private=&a, .dev=&d}; struct drm_minor m={.dev=&drm};
    struct drm_file p={.minor=&m}; struct file f={.private_data=&p}; struct inode i={0};
    resume_rc=-EIO;
    if (ane_drm_release(&i, &f) || releases != 1 || recoveries || put_count) {
        fprintf(stderr, "failed resume: releases=%d recoveries=%d gets=%d puts=%d\n", releases, recoveries, get_count, put_count); return 1;
    }
    releases=recoveries=get_count=put_count=0; resume_rc=0;
    if (ane_drm_release(&i, &f) || releases != 1 || recoveries != 1 || get_count != put_count) {
        fprintf(stderr, "successful resume: releases=%d recoveries=%d gets=%d puts=%d\n", releases, recoveries, get_count, put_count); return 1;
    }
    releases=recoveries=get_count=put_count=0; a.removed=true;
    if (ane_drm_release(&i, &f) || releases != 1 || recoveries || get_count != put_count) {
        fprintf(stderr, "removed device: releases=%d recoveries=%d gets=%d puts=%d\n",
                releases, recoveries, get_count, put_count); return 1;
    }
    puts("runtime PM close: PASS"); return 0;
}
'''
with tempfile.TemporaryDirectory() as tmp:
    binary = pathlib.Path(tmp) / 'ane-close-test'
    subprocess.run(['cc', '-std=gnu11', '-x', 'c', '-o', str(binary), '-'],
                   input=prefix + post + release + main, text=True, check=True)
    subprocess.run([str(binary)], check=True)

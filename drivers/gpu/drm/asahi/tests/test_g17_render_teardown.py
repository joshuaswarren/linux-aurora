#!/usr/bin/env python3
"""Run the production render cancellation callback with a mocked fence wait."""
from pathlib import Path
import argparse
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument('--baseline-ref')
args = parser.parse_args()
current = (root / 'g17_drm.rs').read_text()
source = subprocess.check_output(['git', '-C', str(root), 'show',
    args.baseline_ref + ':drivers/gpu/drm/asahi/g17_drm.rs'], text=True) if args.baseline_ref else current

def item(source, marker):
    start = source.index(marker)
    end = source.index('{', start) + 1
    depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

wait = item(current, 'fn wait_for_render_teardown(')
backend = item(source, 'impl g17_job::G17PRenderWatchBackend for G17PRenderWatchBackend {')
if args.baseline_ref:
    # Adapt only the old timeout signature to the identity-aware mock sink.
    backend = backend.replace('packet.submission_id(), ETIMEDOUT,',
                              'packet.submission_id(), packet.completion(), ETIMEDOUT,')
    backend = backend.replace('packet.submission_id(), ECANCELED,',
                              'packet.submission_id(), packet.completion(), ECANCELED,')

body = r'''
#![allow(dead_code)]
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
static HELD: AtomicBool = AtomicBool::new(false);
static WAIT_RESULT: AtomicI64 = AtomicI64::new(1);
static WAITS: AtomicUsize = AtomicUsize::new(0);
const ETIMEDOUT: i32 = -110;
const ECANCELED: i32 = -125;
struct Fence;
impl Fence {fn raw(&self) -> usize {1}}
mod kernel {
    pub mod time {pub fn msecs_to_jiffies(ms: u32) -> u64 {ms.into()}}
    pub mod drm {pub mod sched {pub enum Status {Nominal}}}
}
mod bindings {
    pub unsafe fn dma_fence_wait_timeout(raw: usize, interruptible: bool, timeout: i64) -> i64 {
        assert_eq!(raw, 1);
        assert!(!interruptible, "fatal process signal must not cancel the retirement wait");
        assert_eq!(timeout, 2000);
        assert!(!super::HELD.load(super::Ordering::SeqCst), "RTKit completion worker needs this lock");
        super::WAITS.fetch_add(1, super::Ordering::SeqCst);
        super::WAIT_RESULT.load(super::Ordering::SeqCst)
    }
}
struct Completion;
mod g17_job {
    use super::*;
    pub struct G17PRenderWatchPacket(pub Arc<Completion>);
    impl G17PRenderWatchPacket {
        pub fn render_slot(&self) -> u8 {1}
        pub fn submission_id(&self) -> u64 {42}
        pub fn completion(&self) -> &Arc<Completion> {&self.0}
        pub fn fence(&self) -> Fence {Fence}
    }
    pub trait G17PRenderWatchBackend {
        fn timed_out(&self, packet: Arc<G17PRenderWatchPacket>) -> kernel::drm::sched::Status;
        fn cancel(&self, packet: Arc<G17PRenderWatchPacket>);
    }
}
#[derive(Default)] struct Runtime {calls: RefCell<Vec<i32>>}
struct Guard<'a>(&'a Runtime);
impl Runtime {
    fn mutex(&self) -> &Self {self}
    fn lock(&self) -> Guard<'_> {
        assert!(!HELD.swap(true, Ordering::SeqCst));
        Guard(self)
    }
}
impl Guard<'_> {
    fn timeout_deferred_render(&mut self, slot:u8, id:u64, _: &Arc<Completion>, error:i32) {
        assert_eq!((slot,id),(1,42));
        self.0.calls.borrow_mut().push(error);
    }
}
impl Drop for Guard<'_> {fn drop(&mut self) {HELD.store(false,Ordering::SeqCst);}}
struct G17PRenderWatchBackend {runtime: Runtime}
WAIT
BACKEND
fn setup(result: i64) -> (G17PRenderWatchBackend, Arc<g17_job::G17PRenderWatchPacket>) {
    WAIT_RESULT.store(result, Ordering::SeqCst);
    WAITS.store(0, Ordering::SeqCst);
    (G17PRenderWatchBackend {runtime: Runtime::default()},
     Arc::new(g17_job::G17PRenderWatchPacket(Arc::new(Completion))))
}
#[test] fn completed_teardown_never_becomes_device_loss() {
    let (b,p) = setup(1);
    g17_job::G17PRenderWatchBackend::cancel(&b,p);
    assert_eq!(WAITS.load(Ordering::SeqCst),1);
    assert!(b.runtime.calls.borrow().is_empty());
}
#[test] fn genuinely_stuck_teardown_still_uses_bounded_failure_path() {
    let (b,p) = setup(0);
    g17_job::G17PRenderWatchBackend::cancel(&b,p);
    assert_eq!(WAITS.load(Ordering::SeqCst),1);
    assert_eq!(*b.runtime.calls.borrow(),[-110]);
}
#[test] fn normal_watchdog_does_not_add_a_second_wait() {
    let (b,p) = setup(0);
    g17_job::G17PRenderWatchBackend::timed_out(&b,p);
    assert_eq!(WAITS.load(Ordering::SeqCst),0);
    assert_eq!(*b.runtime.calls.borrow(),[-110]);
}
'''.replace('WAIT\n', wait + '\n').replace('BACKEND\n', backend + '\n')
with tempfile.TemporaryDirectory(prefix='g17-render-teardown-') as directory:
    exe = str(Path(directory) / 'test')
    subprocess.run(['rustc', '--edition=2021', '--test', '-', '-o', exe], input=body, text=True, check=True)
    subprocess.run([exe, '--test-threads=1'], check=True)
print('PASS render teardown waits outside the runtime lock; normal watchdog unchanged')

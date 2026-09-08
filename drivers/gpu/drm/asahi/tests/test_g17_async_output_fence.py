#!/usr/bin/env python3
"""Run the production output-fence publication against an asynchronous job.

A compute submission is queued, not executed, by the time its ioctl returns.
The fence installed into the caller's output syncobjs therefore has to be the
scheduler's completion fence -- publishing the submit path's own fence, whether
as success or as the guard's ECANCELED failure, tells a DRM syncobj wait that
work which has not run is finished, and the client reads its destination buffer
before the GPU has filled it.

The struct, its Drop and `install_output_fences` are lifted verbatim from
g17_drm.rs, so this exercises the shipped code rather than a restatement.
"""
from pathlib import Path
import subprocess, tempfile

source = (Path(__file__).resolve().parents[1] / 'g17_drm.rs').read_text()
live_source = (Path(__file__).resolve().parents[1] / 'g17_live_boot.rs').read_text()


def block(marker):
    a = source.index(marker)
    e = source.index('{', a) + 1
    depth = 1
    while depth:
        depth += (source[e] == '{') - (source[e] == '}')
        e += 1
    return source[a:e]


install = block('    fn install_output_fences(')
guard_struct = block('struct G17POutputFencePublication<')
guard_impl = block("impl G17POutputFencePublication<'_> {")
guard_drop = block("impl Drop for G17POutputFencePublication<'_> {")
render_completion = block("impl G17PRenderCompletion {")

complete = render_completion[render_completion.index("pub(crate) fn complete"):]
assert complete.index("compare_exchange(0, 1") < complete.index("self.fence.signal();"), \
    'render completion does not reject duplicate firmware notifications'
assert complete.index("drop(self.lease.lock().take());") < complete.index("self.fence.signal();"), \
    'render completion wakes userspace before releasing its physical slot lease'

# The finite scheduler timeout is the only non-event completion fallback. It
# must inspect final device state before consuming the once-only settlement
# gate, so an early event at a stamp rollover cannot become device loss.
timeout_start = live_source.index("    pub(crate) fn timeout_deferred_render(")
timeout_end = live_source.index("    pub(crate) fn abandon_deferred_render(", timeout_start)
timeout = live_source[timeout_start:timeout_end]
assert timeout.index("self.poll_deferred_render_completion(render_slot)") \
    < timeout.index("self.render_fence_state.settle(render_slot)"), \
    'watchdog consumes the settlement gate before its final completion observation'
assert "signal.complete(Ok(observed));" in timeout, \
    'watchdog cannot salvage a completion that became visible after the event edge'
assert ") -> bool {" in timeout and "return !retired;" in timeout, \
    'watchdog does not report whether firmware still owns the slot'

# Cancellation and submit-error cleanup must classify the final hardware state
# before permanently retiring a physical slot. A successful one-frame process
# can close while its already-completed watch job is still queued.
watch = block("impl g17_job::G17PRenderWatchBackend for G17PRenderWatchBackend {")
for marker in ("fn timed_out(", "fn cancel("):
    body = watch[watch.index(marker):]
    body = body[:body.index("\n    }", body.index("{"))]
    assert body.index("timeout_deferred_render(") < body.index("quarantine_render_slot("), marker
    assert "if quarantine" in body, marker

render_submit_start = source.index("let submit = self.runtime")
render_submit_end = source.index("publication.publish_async(&finished);", render_submit_start)
render_submit = source[render_submit_start:render_submit_end]
assert render_submit.count("if quarantine") == 3
assert render_submit.count("timeout_deferred_render(") == 3

# The submit path must hand the guard to the asynchronous compute path, and the
# asynchronous path must publish through it only after the job is enqueued.
submit = source[source.index('impl queue::Queue for G17PQueue {'):]
assert 'publication: &mut G17POutputFencePublication' in source, \
    'submit_compute_async does not receive the output-fence guard'
assert '&mut publication,' in submit, \
    'submit() does not hand the guard to the asynchronous compute path'
enqueue = source.index('.enqueue(packet, dependencies)?;')
assert enqueue < source.index('publication.publish_async(&finished);', enqueue), \
    'the completion fence is published before the job is enqueued'

preamble = r'''
#![allow(dead_code, unused_variables)]
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Copy, Clone, PartialEq)]
struct Error(i32);
const ECANCELED: Error = Error(125);

/// One dma_fence: unsignalled until whoever owns the work signals it.
#[derive(Default)]
struct FenceState {
    signalled: AtomicBool,
    error: AtomicI32,
}

#[derive(Clone)]
struct Fence(Arc<FenceState>);

impl Fence {
    fn new() -> Self {
        Self(Arc::new(FenceState::default()))
    }
    fn from_fence<T>(user: &UserFence<T>) -> Fence {
        Fence(user.0.clone())
    }
    fn signal(&self) {
        self.0.signalled.store(true, Ordering::SeqCst);
    }
    /// What a drm_syncobj wait observes: an error fence counts as signalled.
    fn is_signalled(&self) -> bool {
        self.0.signalled.load(Ordering::SeqCst)
    }
    fn error(&self) -> i32 {
        self.0.error.load(Ordering::SeqCst)
    }
}

struct UserFence<T>(Arc<FenceState>, PhantomData<T>);

impl<T> UserFence<T> {
    fn new() -> Self {
        Self(Arc::new(FenceState::default()), PhantomData)
    }
    fn set_error(&self, error: Error) {
        self.0.error.store(error.0, Ordering::SeqCst);
    }
    fn signal(&self) {
        self.0.signalled.store(true, Ordering::SeqCst);
    }
}

struct G17PSubmissionFence;

#[derive(Clone, Default)]
struct Syncobj(Arc<Mutex<Option<Fence>>>);

impl Syncobj {
    fn replace_fence(&self, fence: Option<&Fence>) {
        *self.0.lock().unwrap() = fence.cloned();
    }
    fn add_point(&self, _chain: Chain, fence: &Fence, _value: u64) {
        *self.0.lock().unwrap() = Some(fence.clone());
    }
    /// What userspace would wait on. `None` means nothing was ever installed,
    /// which hangs a waiter forever.
    fn installed(&self) -> Option<Fence> {
        self.0.lock().unwrap().clone()
    }
}

struct Chain;

mod file {
    pub(super) struct SyncItem {
        pub syncobj: super::Syncobj,
        pub chain_fence: Option<super::Chain>,
        pub timeline_value: u64,
    }
}

type KVec<T> = Vec<T>;

struct G17PQueue;

fn out_sync(syncobj: &Syncobj) -> file::SyncItem {
    file::SyncItem {
        syncobj: syncobj.clone(),
        chain_fence: None,
        timeline_value: 0,
    }
}

/// The submit ioctl as it stands before it knows render from compute: split
/// the output syncobjs off and arm the guard over them.
fn arm<'a>(
    early: &'a UserFence<G17PSubmissionFence>,
    outputs: KVec<file::SyncItem>,
) -> G17POutputFencePublication<'a> {
    G17POutputFencePublication {
        fence: early,
        outputs,
        succeeded: false,
    }
}
'''

tests = r'''
/// The asynchronous compute path: the job is queued here and runs later.
#[test]
fn queued_compute_publishes_the_job_fence_not_the_ioctl_fence() {
    let syncobj = Syncobj::default();
    let early = UserFence::<G17PSubmissionFence>::new();
    let finished = Fence::new();
    {
        let mut publication = arm(&early, vec![out_sync(&syncobj)]);
        submit_async(&mut publication, &finished);
    }

    let installed = syncobj.installed().expect(
        "no fence installed into the output syncobj: a waiter hangs forever",
    );
    assert!(
        !installed.is_signalled(),
        "the output syncobj was signalled when the ioctl returned, before the \
         queued job ran: userspace reads its destination buffer stale",
    );

    // The job runs.
    finished.signal();
    assert!(installed.is_signalled());
    assert_eq!(installed.error(), 0);
}

/// A failure before the job is queued must still publish, and must say so.
#[test]
fn refused_compute_publishes_a_cancelled_fence() {
    let syncobj = Syncobj::default();
    let early = UserFence::<G17PSubmissionFence>::new();
    {
        let _publication = arm(&early, vec![out_sync(&syncobj)]);
        // enqueue failed: the guard is never handed the job fence.
    }
    let installed = syncobj.installed().expect("nothing published on failure");
    assert!(installed.is_signalled());
    assert_eq!(installed.error(), ECANCELED.0);
}

/// The synchronous render path is unchanged: the work is done, so the ioctl's
/// own fence is the right answer.
#[test]
fn completed_render_publishes_a_signalled_fence() {
    let syncobj = Syncobj::default();
    let early = UserFence::<G17PSubmissionFence>::new();
    {
        let mut publication = arm(&early, vec![out_sync(&syncobj)]);
        publication.succeeded();
    }
    let installed = syncobj.installed().expect("nothing published on success");
    assert!(installed.is_signalled());
    assert_eq!(installed.error(), 0);
}
'''

fixed_async = '''
fn submit_async(publication: &mut G17POutputFencePublication<'_>, finished: &Fence) {
    publication.publish_async(finished);
}
'''

# Exactly what the async path did before this fix: the outputs had already been
# split off into the guard, so `install_output_fences` ran over an empty list
# and the guard published its own fence on the way out.
regressed_async = '''
fn submit_async(publication: &mut G17POutputFencePublication<'_>, finished: &Fence) {
    let syncs: KVec<file::SyncItem> = KVec::new();
    G17PQueue::install_output_fences(syncs.into_iter(), finished);
}
'''

body = (preamble + guard_struct + '\n' + guard_impl + '\n' + guard_drop
        + '\nimpl G17PQueue {\n' + install + '\n}\n')

with tempfile.TemporaryDirectory() as d:
    d = Path(d)
    good = d / 'good.rs'
    good.write_text(body + fixed_async + tests)
    exe = d / 'good'
    subprocess.run(['rustc', '--edition=2021', '--test', str(good), '-o', str(exe)],
                   check=True)
    subprocess.run([str(exe), '--test-threads=1'], check=True)

    bad = d / 'bad.rs'
    bad.write_text(body + regressed_async + tests)
    badexe = d / 'bad'
    subprocess.run(['rustc', '--edition=2021', '--test', str(bad), '-o', str(badexe)],
                   check=True)
    result = subprocess.run(
        [str(badexe), 'queued_compute_publishes_the_job_fence_not_the_ioctl_fence'],
        text=True, capture_output=True, timeout=60)
    assert result.returncode != 0, 'the pre-fix async path passed; the test proves nothing'
    assert 'before the queued job ran' in result.stdout, result.stdout + result.stderr

print('PASS production output-fence publication: queued work publishes the scheduler '
      'completion fence; render releases its slot before wake and duplicate firmware '
      'notifications signal once')

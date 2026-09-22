#!/usr/bin/env python3
"""Compile the real watchdog entry point against a mock settlement sink."""
from pathlib import Path
import argparse
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument('--baseline-ref', help='Run the same ownership assertions against an older implementation')
args = parser.parse_args()
source = (subprocess.check_output(['git', '-C', str(root), 'show',
          args.baseline_ref + ':drivers/gpu/drm/asahi/g17_live_boot.rs'], text=True)
          if args.baseline_ref else (root / 'g17_live_boot.rs').read_text())
start = source.index('    pub(crate) fn timeout_deferred_render(')
end = source.index('{', start) + 1
depth = 1
while depth:
    depth += (source[end] == '{') - (source[end] == '}')
    end += 1
method = source[start:end]
if args.baseline_ref and 'completion:' not in method:
    # Adapt only the signature so the same test invokes the old body. The
    # deliberately unused argument exposes its missing ownership check.
    method = method.replace('submission_id: u64,',
        'submission_id: u64, completion: &Arc<crate::g17_drm::G17PRenderCompletion>,')
body = r'''
#![allow(dead_code)]
use std::sync::Arc;
type Error = i32;
mod g17_drm { pub struct G17PRenderCompletion; }
use g17_drm::G17PRenderCompletion;
struct Runtime {
    pending_render: [Option<Arc<G17PRenderCompletion>>; 2],
    pending_render_queued: [Option<Arc<G17PRenderCompletion>>; 2],
    settled: Vec<(u8, Option<u64>, Error)>,
}
impl Runtime {
    fn settle_failed_deferred_render(&mut self, slot: u8, id: Option<u64>,
                                    stamp: Option<u8>, error: Error) {
        assert!(stamp.is_none());
        self.settled.push((slot, id, error));
        self.pending_render[slot as usize] = None;
    }
METHOD
}
#[test]
fn completed_watch_cannot_cancel_a_reused_slot() {
    let old = Arc::new(G17PRenderCompletion);
    let next = Arc::new(G17PRenderCompletion);
    let mut r = Runtime {pending_render: [Some(next.clone()), None], pending_render_queued: [None, None], settled: vec![]};
    // The callback must validate the actual object, not just a log identifier.
    r.timeout_deferred_render(0, 7, &old, -125);
    r.timeout_deferred_render(0, 7, &old, -110);
    assert!(r.settled.is_empty());
    assert!(Arc::ptr_eq(r.pending_render[0].as_ref().unwrap(), &next));
    r.timeout_deferred_render(0, 7, &next, -110);
    assert_eq!(r.settled, [(0, Some(7), -110)]);
}
#[test]
fn duplicate_and_invalid_callbacks_do_not_settle_anything() {
    let c = Arc::new(G17PRenderCompletion);
    let mut r = Runtime {pending_render: [None, Some(c.clone())], pending_render_queued: [None, None], settled: vec![]};
    r.timeout_deferred_render(0, 9, &c, -125);
    r.timeout_deferred_render(255, 9, &c, -125);
    assert!(r.settled.is_empty());
    r.timeout_deferred_render(1, 9, &c, -125);
    r.timeout_deferred_render(1, 9, &c, -125);
    assert_eq!(r.settled, [(1, Some(9), -125)]);
}
'''.replace('METHOD', method)
with tempfile.TemporaryDirectory(prefix='g17-watch-identity-') as directory:
    exe = str(Path(directory) / 'test')
    subprocess.run(['rustc', '--edition=2021', '--test', '-', '-o', exe],
                   input=body, text=True, check=True)
    subprocess.run([exe], check=True)

if not args.baseline_ref:
    drm = (root / 'g17_drm.rs').read_text()
    assert drm.count('packet.submission_id(), packet.completion(),') == 2
    assert drm.count('render_slot, submission_id, completion, error,') == 1
    assert drm.count('render_slot, packet.submission_id(), packet.completion(), ETIMEDOUT,') == 2
print('PASS exact completion identity on every deferred-render failure entry')

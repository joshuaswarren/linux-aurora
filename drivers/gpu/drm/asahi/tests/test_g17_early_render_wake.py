#!/usr/bin/env python3
"""Run the production wake gate against submitted-work and transport states."""
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[1]
runtime = (root / 'g17_live_boot.rs').read_text()
start = runtime.index('    pub(crate) fn wake_pending_render_submission(')
pos = runtime.index('{', start) + 1
depth = 1
while depth:
    depth += (runtime[pos] == '{') - (runtime[pos] == '}')
    pos += 1
method = runtime[start:pos]
source = r'''
use std::pin::Pin;
type Result<T=()> = std::result::Result<T, i32>;
const EIO:i32=5;const ENODEV:i32=19;
mod g17_rtkit {pub const ENDPOINT_GFX_INTERRUPTS:u8=0x21;}
#[derive(Default)] struct Transport {sent:Vec<(u8,u64)>,error:Option<i32>}
impl Transport {fn send_message(mut self:Pin<&mut Self>,ep:u8,note:u64)->Result {
 if let Some(e)=self.error{return Err(e);}
 self.sent.push((ep,note));Ok(())
}}
struct Queue {active:Option<()>}
struct Runtime {
 crashed:bool,pending_compute:Option<()>,pending_render:Vec<Option<()>>,
 pending_render_queued:Vec<Vec<()>>,async_compute_queues:Vec<Queue>,
 quarantined_render_completions:Vec<()>,gfx_rtkit:Option<Transport>
}
impl Runtime {
 fn is_crashed(&self)->bool {self.crashed}
 fn new()->Self {Self {crashed:false,pending_compute:None,pending_render:vec![None;4],
 pending_render_queued:vec![vec![];4],async_compute_queues:vec![Queue{active:None}],
 quarantined_render_completions:vec![],gfx_rtkit:Some(Transport::default())}}
''' + method + r'''
}
#[test] fn no_submitted_work_sends_exactly_the_normal_scan_note() {
 let mut r=Runtime::new();assert_eq!(r.wake_pending_render_submission(),Ok(()));
 assert_eq!(r.gfx_rtkit.unwrap().sent,vec![(0x21,0x0083_0000_0000_0000)]);
}
#[test] fn every_submitted_work_witness_suppresses_the_extra_scan() {
 for state in 0..5 {
  let mut r=Runtime::new();
  match state {0=>r.pending_compute=Some(()),1=>r.pending_render[3]=Some(()),
   2=>r.pending_render_queued[2].push(()),3=>r.async_compute_queues[0].active=Some(()),
   _=>r.quarantined_render_completions.push(())}
  assert_eq!(r.wake_pending_render_submission(),Ok(()));
  assert!(r.gfx_rtkit.unwrap().sent.is_empty());
 }
}
#[test] fn failure_never_becomes_a_successful_wake() {
 let mut r=Runtime::new();r.crashed=true;
 assert_eq!(r.wake_pending_render_submission(),Err(EIO));
 assert!(r.gfx_rtkit.as_ref().unwrap().sent.is_empty());
 r.crashed=false;r.gfx_rtkit=None;
 assert_eq!(r.wake_pending_render_submission(),Err(ENODEV));
 r.gfx_rtkit=Some(Transport {error:Some(110),..Transport::default()});
 assert_eq!(r.wake_pending_render_submission(),Err(110));
 assert!(r.gfx_rtkit.unwrap().sent.is_empty());
}
'''
with tempfile.TemporaryDirectory(prefix='g17-early-wake-') as tmp:
    p=Path(tmp);(p/'test.rs').write_text(source)
    subprocess.run(['rustc','--edition=2021','--test',str(p/'test.rs'),'-o',str(p/'test')],check=True)
    subprocess.run([str(p/'test')],check=True)

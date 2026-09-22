#!/usr/bin/env python3
"""Queue order crosses G17P render/compute engines without global serialization."""
from pathlib import Path
import subprocess
import tempfile


root = Path(__file__).resolve().parents[1]
source = (root / "g17_drm.rs").read_text()
manager = (root / "g17_manager.rs").read_text()


def function(text, marker):
    start = text.index(marker)
    pos = text.index("{", start) + 1
    depth = 1
    while depth:
        depth += (text[pos] == "{") - (text[pos] == "}")
        pos += 1
    return text[start:pos]


assert "last_submission: Option<Fence>" in source
assert "last_submission_engine: Option<G17PSubmissionEngine>" in source
assert "last_async_compute" not in source

compute = function(source, "    fn submit_compute_async(")
assert compute.index("self.last_submission_engine != Some(G17PSubmissionEngine::Compute)") < compute.index(
    ".enqueue(packet, dependencies)?"
)
assert compute.index("self.last_submission = Some(finished.clone());") < compute.index(
    "publication.publish_async(&finished);"
)
assert "self.last_submission_engine = Some(G17PSubmissionEngine::Compute);" in compute

render = function(source, "    fn submit_render_async(")
assert render.index("self.last_submission_engine != Some(G17PSubmissionEngine::Render)") < render.index(
    ".enqueue(packet, dependencies)?"
)
assert render.index("self.last_submission = Some(finished.clone());") < render.index(
    "publication.publish_async(&finished);"
)
assert "self.last_submission_engine = Some(G17PSubmissionEngine::Render);" in render
assert "translated.render_barrier = header.render_barrier;" in source
assert "translated.compute_barrier = header.compute_barrier;" in source
assert "join_queue_timeline" not in source
assert "acquire_ready_lease" not in source

wait = function(source, "    fn wait_fence(")
assert "dma_fence_wait_timeout" in wait and "MAX_SCHEDULE_TIMEOUT" in wait
assert wait.index("dma_fence_wait_timeout") < wait.index("dma_fence_get_status")
assert "if status < 0" in wait, "a signalled error fence incorrectly satisfies queue order"
backend = function(source, "impl g17_job::G17PRenderQueueBackend for G17PRenderQueueBackend")
assert backend.index("for fence in packet.input_fences()") < backend.index(
    "self.registry.acquire_lease(self.owner)?"
)

# Same-engine host fences are intentionally absent, but a requested UAPI VDM
# barrier must survive into the owned packet and become the previous 3D stamp
# in the next TA kick. Per-engine predecessor history alone does not create
# this cross-stage edge.
stage = function(manager, "    fn stage_render_groups_with_sksm(")
assert "wait_previous_fragment: bool" in stage
assert "queues.fragment.producer.last_add_kicks_timestamp" in stage
assert "g17p_render_barrier_from_timestamp" in stage
assert stage.index("previous_fragment_barrier") < stage.index(
    "publish_render_sksm_group(dev, true, tiling_queue, previous_fragment_barriers)"
)
begin = function(manager, "    fn begin_translated_render_with_hardware_buffer(")
assert "command.render_barrier != g17_uapi::UAPI_BARRIER_NONE" in begin

# Host model of exactly the state transition above. A logical queue carries a
# single cross-engine predecessor; a different logical queue has no dependency
# and therefore remains eligible for hardware overlap.
rust = r'''
#[derive(Clone,Copy,Debug,PartialEq,Eq)] struct Fence(u32);
#[derive(Clone,Copy,Debug,PartialEq,Eq)] enum Engine { Render, Compute }
#[derive(Default)] struct Queue { last: Option<Fence>, engine: Option<Engine>, next: u32, fragment_stamp: u64 }
impl Queue {
 fn render(&mut self, publish: bool, wait_fragment: bool) -> (Option<Fence>, Option<Fence>, Option<u64>) {
  let dependency = if self.engine == Some(Engine::Render) { None } else { self.last };
  let gpu_barrier = if wait_fragment && self.fragment_stamp != 0 { Some(self.fragment_stamp) } else { None };
  if !publish { return (dependency, None, gpu_barrier); }
  self.next += 1; let out = Fence(self.next); self.last = Some(out); self.engine=Some(Engine::Render);
  self.fragment_stamp += 1;
  (dependency, Some(out), gpu_barrier)
 }
 fn compute(&mut self, publish: bool) -> (Option<Fence>, Option<Fence>) {
  let dependency = if self.engine == Some(Engine::Compute) { None } else { self.last };
  if !publish { return (dependency, None); }
  self.next += 1; let out = Fence(self.next); self.last = Some(out); self.engine=Some(Engine::Compute);
  (dependency, Some(out))
 }
}
#[test] fn render_compute_compute_render_is_one_timeline() {
 let mut q=Queue::default();
 let (_,r1,_)=q.render(true,true); let r1=r1.unwrap();
 let (d,c1)=q.compute(true); assert_eq!(d,Some(r1)); let c1=c1.unwrap();
 let (d,c2)=q.compute(true); assert_eq!(d,None); let c2=c2.unwrap();
 let (w,r2,_)=q.render(true,true); assert_eq!(w,Some(c2)); assert!(r2.is_some());
}
#[test] fn consecutive_renders_use_firmware_barrier_without_host_fence() {
 let mut q=Queue::default();let (_,r1,b1)=q.render(true,true);assert!(r1.is_some());assert_eq!(b1,None);
 let (dependency,r2,b2)=q.render(true,true);assert_eq!(dependency,None);assert!(r2.is_some());assert_eq!(b2,Some(1));
 let (_,r3,b3)=q.render(true,false);assert!(r3.is_some());assert_eq!(b3,None);
}
#[test] fn rejected_submit_preserves_the_predecessor() {
 let mut q=Queue::default(); let (_,r,_)=q.render(true,true); let r=r.unwrap();
 assert_eq!(q.compute(false),(Some(r),None));
 assert_eq!(q.last,Some(r));
}
#[test] fn independent_queues_have_no_artificial_dependency() {
 let mut a=Queue::default(); let mut b=Queue::default();
 let (_,ar,_)=a.render(true,true); assert!(ar.is_some());
 let (bd,br,_)=b.render(true,true); assert_eq!(bd,None); assert!(br.is_some());
}
'''
with tempfile.TemporaryDirectory() as d:
    d = Path(d)
    src = d / "cross.rs"
    src.write_text(rust)
    exe = d / "cross"
    subprocess.run(["rustc", "--edition=2021", "--test", str(src), "-o", str(exe)], check=True)
    subprocess.run([str(exe), "--test-threads=1"], check=True, timeout=60)

print("PASS cross-engine queue order: render/compute share one logical timeline; independent queues overlap")

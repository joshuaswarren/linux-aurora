#!/usr/bin/env python3
"""Queue order crosses G17P render/compute engines without global serialization."""
from pathlib import Path
import subprocess
import tempfile


root = Path(__file__).resolve().parents[1]
source = (root / "g17_drm.rs").read_text()


def function(text, marker):
    start = text.index(marker)
    pos = text.index("{", start) + 1
    depth = 1
    while depth:
        depth += (text[pos] == "{") - (text[pos] == "}")
        pos += 1
    return text[start:pos]


assert "last_submission: Option<Fence>" in source
assert "last_async_compute" not in source

compute = function(source, "    fn submit_compute_async(")
assert compute.index("if let Some(prior) = self.last_submission.as_ref()") < compute.index(
    ".enqueue(packet, dependencies)?"
)
assert compute.index("self.last_submission = Some(finished.clone());") < compute.index(
    "publication.publish_async(&finished);"
)

submit = function(source, "    fn submit_guarded(")
render = submit[submit.index("self.join_queue_timeline(submission_id)?;"):]
assert render.index("self.join_queue_timeline(submission_id)?;") < render.index(
    "let lease = self.acquire_ready_lease"
)
assert render.index("self.last_submission = Some(finished.clone());") < render.index(
    "publication.publish_async(&finished);"
)

join = function(source, "    fn join_queue_timeline(")
assert "dma_fence_wait_timeout" in join
assert join.index("dma_fence_wait_timeout") < join.index("self.last_submission = None;")
assert "MAX_SCHEDULE_TIMEOUT" in join

# Host model of exactly the state transition above. A logical queue carries a
# single cross-engine predecessor; a different logical queue has no dependency
# and therefore remains eligible for hardware overlap.
rust = r'''
#[derive(Clone,Copy,Debug,PartialEq,Eq)] struct Fence(u32);
#[derive(Default)] struct Queue { last: Option<Fence>, next: u32 }
impl Queue {
 fn render(&mut self, publish: bool) -> (Option<Fence>, Option<Fence>) {
  let waited = self.last.take();
  if !publish { self.last = waited; return (waited, None); }
  self.next += 1; let out = Fence(self.next); self.last = Some(out);
  (waited, Some(out))
 }
 fn compute(&mut self, publish: bool) -> (Option<Fence>, Option<Fence>) {
  let dependency = self.last;
  if !publish { return (dependency, None); }
  self.next += 1; let out = Fence(self.next); self.last = Some(out);
  (dependency, Some(out))
 }
}
#[test] fn render_compute_compute_render_is_one_timeline() {
 let mut q=Queue::default();
 let (_,r1)=q.render(true); let r1=r1.unwrap();
 let (d,c1)=q.compute(true); assert_eq!(d,Some(r1)); let c1=c1.unwrap();
 let (d,c2)=q.compute(true); assert_eq!(d,Some(c1)); let c2=c2.unwrap();
 let (w,r2)=q.render(true); assert_eq!(w,Some(c2)); assert!(r2.is_some());
}
#[test] fn rejected_submit_preserves_the_predecessor() {
 let mut q=Queue::default(); let (_,r)=q.render(true); let r=r.unwrap();
 assert_eq!(q.compute(false),(Some(r),None));
 assert_eq!(q.last,Some(r));
}
#[test] fn independent_queues_have_no_artificial_dependency() {
 let mut a=Queue::default(); let mut b=Queue::default();
 let (_,ar)=a.render(true); assert!(ar.is_some());
 let (bd,br)=b.render(true); assert_eq!(bd,None); assert!(br.is_some());
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

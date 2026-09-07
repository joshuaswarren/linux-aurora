#!/usr/bin/env python3
from pathlib import Path


source = (Path(__file__).resolve().parents[1] / "g17_drm.rs").read_text()

assert "last_async_compute: Option<Fence>" in source
assert "self.last_async_compute = Some(finished);" in source
assert "fn join_async_compute_timeline" in source

render_dispatch = source.index(
    "self.join_async_compute_timeline(submission_id)?;"
)
lease = source.index(
    "let lease = self.acquire_ready_lease",
    render_dispatch,
)
output_fence = source.index(
    "let output_fence: UserFence<G17PSubmissionFence>",
    lease,
)

assert render_dispatch < lease < output_fence

wait = source[source.index("fn wait_render_slot("):source.index("/// Give the hardware", source.index("fn wait_render_slot(")) if "/// Give the hardware" in source[source.index("fn wait_render_slot("):] else source.index("impl Drop for G17PComputeLeaseGuard")]
assert ".poll_deferred_render_completion(render_slot)?" in wait
assert wait.index(".poll_deferred_render_completion(render_slot)?") < wait.index("fsleep(Delta::from_millis(1))")
render_call = source[source.index(".submit_translated_render(", render_dispatch):]
assert "lease.slot(),\n                        true," in render_call[:400]
assert ".wait_render_slot(lease.slot())" in render_call[:1200]
print("PASS: render joins its compute timeline, publishes by bounded slot, then polls with the runtime mutex released between observations")

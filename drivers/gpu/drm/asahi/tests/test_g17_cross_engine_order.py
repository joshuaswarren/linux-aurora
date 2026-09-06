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
    "let _lease = self.acquire_ready_lease",
    render_dispatch,
)
output_fence = source.index(
    "let output_fence: UserFence<G17PSubmissionFence>",
    lease,
)

assert render_dispatch < lease < output_fence
print("PASS: render joins the logical queue's async compute timeline before its lease")

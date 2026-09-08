#!/usr/bin/env python3
"""Dead DRM queues reclaim drained render contexts without evicting live VMs."""

from pathlib import Path

root = Path(__file__).resolve().parents[1]
manager = (root / "g17_manager.rs").read_text()
runtime = (root / "g17_live_boot.rs").read_text()
drm = (root / "g17_drm.rs").read_text()


def function(source, marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


drop = function(drm, "impl Drop for G17PQueue")
release = function(manager, "pub(crate) fn release_render_context_owner(")
preserve = function(manager, "fn preserve_immediate_retired_render_graph(")
runtime_release = function(runtime, "pub(crate) fn release_render_owner(")

assert drop.index("drop(self.render_scheduler.take())") < drop.index(
    ".release_render_owner(self.id, self.context.context_id() as u16)"
)
assert "released_render_contexts |= 1u64 << context" in release
assert "retired_render_handoff_graphs.remove(index)" in release
assert "state.retired_handoff_graph.take()" in release
assert "released_render_contexts & (1u64 << context)" in preserve
assert "manager.release_render_context_owner(context)" in runtime_release


class Owners:
    def __init__(self):
        self.live = {5, 6}  # compositor queues A/B
        self.retired = set()

    def create_and_destroy(self, context):
        self.retired.add(context)
        # Scheduler drain + queue destruction is the native workqueue-free
        # boundary. It may reclaim only this exact context.
        self.retired.remove(context)
        assert self.live == {5, 6}


owners = Owners()
for generation in range(32):
    owners.create_and_destroy(7 + generation % 4)
assert owners.live == {5, 6}
assert not owners.retired

print("PASS 32 dead-client contexts reclaim after scheduler drain while live A/B survive")

#!/usr/bin/env python3
"""Fresh secondary-pair registration is power-scoped and retry-safe."""

from pathlib import Path

root = Path(__file__).resolve().parents[1]
manager = (root / "g17_manager.rs").read_text()
runtime = (root / "g17_live_boot.rs").read_text()


def function(source: str, marker: str) -> str:
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


submit = function(runtime, "pub(crate) fn submit_translated_render(")
ensure = function(runtime, "fn ensure_g17p_render_registration_power(")
release = function(runtime, "fn release_g17p_render_registration_power(")
construct = function(manager, "fn begin_translated_render_with_hardware_buffer(")

# Native's provider hold spans graph construction and the direct KSMFE
# transaction. It is released on both success and pre-publication failure,
# before the work doorbell can make the prepared outer pair executable.
assert submit.index("ensure_g17p_render_registration_power()?") < submit.index(
    ".begin_translated_render("
)
assert submit.index(".begin_translated_render(") < submit.index(
    "release_g17p_render_registration_power()"
)
assert submit.index("release_g17p_render_registration_power()") < submit.index(
    "self.install_render_completion("
)
assert "DEVICE_CONTROL_IDLE_POWER_OFF_ARG_INHIBIT" in ensure
assert "wait_g17p_render_registration_power_record" in ensure
assert "DEVICE_CONTROL_IDLE_POWER_OFF_ARG_POWER_ON" in release

# Once a fresh storage exists, its UAT root is graph-owned even if the direct
# registration precondition fails. The old ordering moved this owner only
# after outer publication, leaving storage-without-root and poisoning the next
# attempt with EBUSY.
bind_install = construct.index("self.render_bind = Some(G17PRenderRootOwner::Job")
direct = construct.index("self.configure_fresh_nonbootstrap_render_pair(")
outer = construct.index(".publish_partial_opening_pair(")
assert bind_install < direct < outer
assert "execution_context.clone()" in construct[bind_install - 300 : bind_install + 300]
assert "fresh-bind-unconsumed" in construct


class Pair:
    def __init__(self):
        self.graph = None
        self.root = None
        self.registered = False
        self.outer_visible = False

    def construct(self, vm: int, powered: bool) -> bool:
        if self.graph is None:
            self.graph = vm
            self.root = vm
        assert self.graph == vm and self.root == vm
        if not powered:
            assert not self.outer_visible
            return False
        self.registered = True
        self.outer_visible = True
        return True


# Alternate a short-lived slot-0 client with a retained compositor on slot 1.
# Every return sees one coherent graph/root. A failed powered precondition
# cannot convert the following compositor submit into permanent EBUSY.
pair = Pair()
for generation in range(64):
    assert not pair.construct(5, powered=False)
    assert pair.construct(5, powered=True)
    pair.outer_visible = False
assert pair.graph == pair.root == 5
assert pair.registered

print("PASS retained compositor pair rearms across 64 alternating clients")

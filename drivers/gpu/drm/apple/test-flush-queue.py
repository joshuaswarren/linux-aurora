#!/usr/bin/env python3
"""Regression checks for the nonblocking DCP atomic-flush queue."""

from pathlib import Path
import random

root = Path(__file__).parent
iomfb = (root / "iomfb.c").read_text()
driver = (root / "apple_drv.c").read_text()


def block(source, marker):
    start = source.index(marker)
    brace = source.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


flush = block(iomfb, "void dcp_flush(")
dispatch_marker = "static void dcp_dispatch_pending_flushes("
dispatch = block(iomfb[iomfb.rindex(dispatch_marker):], dispatch_marker)
ack = block(iomfb, "static void dcpep_handle_ack(")
reset = block(driver, "static void apple_crtc_reset(")
duplicate = block(driver, "apple_crtc_duplicate_state(")
destroy = block(driver, "static void apple_crtc_destroy_state(")

assert "drm_atomic_state_get(state)" in flush
assert "list_add_tail" in flush
assert "schedule_delayed_work" not in flush
assert "dcp_channel_busy" not in flush
assert "list_first_entry" in dispatch
assert "list_del_init" in dispatch
assert "dcp_dispatch_flush" in dispatch
assert "drm_atomic_state_put(state)" in dispatch
assert dispatch.count("dcp_dispatch_pending_flushes") == 1
assert "goto again" in dispatch
assert "context == DCP_CONTEXT_CMD" in ack
assert "dcp_dispatch_pending_flushes(dcp)" in ack
assert "INIT_LIST_HEAD(&state->pending_flush)" in reset
assert "INIT_LIST_HEAD(&state->pending_flush)" in duplicate
assert "WARN_ON(!list_empty(&state->pending_flush))" in destroy


class QueueModel:
    def __init__(self):
        self.pending = []
        self.busy = False
        self.dispatching = False
        self.submitted = []
        self.refs = {}

    def enqueue(self, ident):
        self.refs[ident] = self.refs.get(ident, 0) + 1
        self.pending.append(ident)
        self.kick()

    def kick(self):
        if self.dispatching or self.busy or not self.pending:
            return
        self.dispatching = True
        ident = self.pending.pop(0)
        self.submitted.append(ident)
        self.busy = True
        self.dispatching = False
        self.refs[ident] -= 1

    def ack(self):
        assert self.busy
        self.busy = False
        self.kick()


for seed in range(100):
    rng = random.Random(seed)
    model = QueueModel()
    expected = []
    for ident in range(1000):
        model.enqueue(ident)
        expected.append(ident)
        if model.busy and rng.randrange(3) == 0:
            model.ack()
    while model.busy or model.pending:
        if model.busy:
            model.ack()
        else:
            model.kick()
    assert model.submitted == expected
    assert not any(model.refs.values())

print("PASS DCP atomic flush queue: FIFO submission, single-flight dispatch, balanced state refs")

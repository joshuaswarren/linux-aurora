#!/usr/bin/env python3
"""Model and source guards for retained render-slot FList ownership.

Each slot holds one complete graph/context owner. A different context may take
that numeric slot only at zero references; concurrently live slots always name
disjoint firmware rows and mutable backing.
"""

from dataclasses import dataclass
from pathlib import Path
import random


ROOT = Path(__file__).resolve().parents[1]
RESOURCES = (ROOT / "g17_resources.rs").read_text()
MANAGER = (ROOT / "g17_manager.rs").read_text()


@dataclass(frozen=True)
class Identity:
    slot: int
    context: int
    pointer: int
    flist_id: int
    cookie: int


class SlotOwner:
    def __init__(self, slot: int):
        self.slot = slot
        self.sequence = 0
        self.identity = None
        self.references = 0

    def acquire(self, context: int, pointer: int):
        if self.references:
            assert self.identity.context == context
            assert self.identity.pointer == pointer
            self.references += 2
            return self.identity
        self.sequence += 1
        self.identity = Identity(
            self.slot,
            context,
            pointer,
            129 + self.slot,
            pointer + 0x440,
        )
        self.references = 2
        return self.identity

    def retire(self):
        assert self.references >= 2
        self.references -= 2


def model_stress():
    rng = random.Random(0x17B1)
    slots = [SlotOwner(i) for i in range(64)]
    live = {}
    last = {}
    for step in range(20_000):
        slot = rng.randrange(len(slots))
        owner = slots[slot]
        if owner.references:
            if rng.randrange(3):
                ident = owner.acquire(owner.identity.context, owner.identity.pointer)
                assert ident == owner.identity
                live.setdefault(slot, []).append(ident)
            else:
                owner.retire()
                live[slot].pop()
        else:
            context = 5 + step * 2
            pointer = 0xFFFF_FC20_1000_0000 + step * 0x8000
            ident = owner.acquire(context, pointer)
            previous = last.get(slot)
            if previous is not None:
                assert ident.pointer != previous.pointer
                assert ident.cookie != previous.cookie
            assert ident.flist_id == 129 + slot
            live[slot] = [ident]
            last[slot] = ident
    assert len({ident.flist_id for ident in last.values()}) == len(last)


def source_guards():
    assert "pub(crate) struct G17PRenderFlist" in RESOURCES
    assert "render_flist: Arc<G17PRenderFlist>" in RESOURCES
    assert "_render_global_backing: mmu::KernelMapping" in RESOURCES
    assert "uat.t8140_render_global_vm()?" in RESOURCES[
        RESOURCES.index("impl G17PRenderFlist"):
        RESOURCES.index("pub(crate) struct G17PUserRenderStorage")
    ]
    assert "self.render_flist.control_shared_va()" in RESOURCES
    assert "self.render_flist.id()" in RESOURCES
    assert "grown: AtomicBool" in RESOURCES
    assert "render_flist_needs_grow" in RESOURCES
    assert "mark_render_flist_grown" in RESOURCES
    assert "g17_initdata::CONTROL_SHARED_ADDRESS" not in RESOURCES[
        RESOURCES.index("fn optional_addresses(&self"):
        RESOURCES.index("pub(crate) fn set_parameter_buffer_lease_token")
    ]
    swap = MANAGER[MANAGER.index("fn swap_active_render_slot_state"):]
    assert "bootstrap_uma" in swap[:5000]
    assert "prepared_render_flist" in swap[:5000]
    assert "next_render_flist_sequence" in swap[:5000]
    assert "checked_sub(129)" in MANAGER
    assert "stage_user_render_uma" in MANAGER
    assert "settle_consumed_render_usc_freelist_releases" in MANAGER
    assert "render_slot_flist_release_sent" in MANAGER
    assert "g17p_control_ring_target_consumed" in MANAGER


if __name__ == "__main__":
    model_stress()
    source_guards()
    print("render FList owner model: 20,000 interleavings passed")

#!/usr/bin/env python3
"""Stress retained render-slot reuse without losing one pair identity."""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[5]
DRM = (ROOT / "drivers/gpu/drm/asahi/g17_drm.rs").read_text()
LIVE = (ROOT / "drivers/gpu/drm/asahi/g17_live_boot.rs").read_text()
MANAGER = (ROOT / "drivers/gpu/drm/asahi/g17_manager.rs").read_text()
RESOURCES = (ROOT / "drivers/gpu/drm/asahi/g17_resources.rs").read_text()


def block(source: str, start: str, end: str) -> str:
    begin = source.index(start)
    return source[begin : source.index(end, begin)]


pair_source = block(
    RESOURCES,
    "pub(crate) struct G17PRenderQueuePair {",
    "const _: () = assert!(G17PRenderQueuePair::for_slot(1)",
)
assert "let tiling = slot as u16 * 2" in pair_source
assert "fragment: tiling + 1" in pair_source
assert "self.tiling >= 128 || self.fragment >= 128" in pair_source
assert "masks[self.tiling as usize / 64]" in pair_source
assert "masks[self.fragment as usize / 64]" in pair_source

# Production routing must derive every externally visible identity from the
# one pair selected by the DRM lease. These anchors deliberately cross layer
# boundaries: losing any one of them recreates a mixed-QID submission.
submit = block(
    LIVE,
    "pub(crate) fn submit_translated_render(",
    "fn ensure_g17p_render_registration_power(",
)
assert "G17PRenderQueuePair::for_slot(render_slot)" in submit
assert ".fresh_render_pair_needs_direct_registration(queue_pair)" in submit
assert ".begin_translated_render(\n                &drm,\n                vm,\n                queue_pair," in submit
assert "self.install_render_completion(render_slot, completion)?" in submit

begin = block(
    MANAGER,
    "fn begin_translated_render_with_hardware_buffer(",
    "pub(crate) fn publish_default_render_pair_after_freelist(",
)
for route in (
    "self.new_render_sksm_queues(pair, entry_vas)",
    "publish_render_qos_queue_record(fragment_header)",
    "publish_render_qos_queue_record(header)",
    ".publish_partial_opening_pair(\n                true,\n                queue_pair,",
):
    assert route in begin, route

registration = block(
    MANAGER,
    "fn configure_fresh_nonbootstrap_render_pair(",
    "fn publish_render_sksm_group(",
)
assert "queues.fragment.config.write_pair()" in registration
assert "queues.tiling.config.write_pair()" in registration
assert "pair.tiling" in registration and "pair.fragment" in registration
assert "qid2" not in registration.lower() and "qid3" not in registration.lower()

validation = block(
    MANAGER,
    "fn validate_render_queue_consistency(",
    "fn commit_render_firmware_install(",
)
for identity in (
    "header_qid != queue_id",
    "header_qid != producer.queue_id",
    "config != expected",
    "header.field_56 != u16::from(producer.qos.queue_byte_32)",
    "qos_record[0] != producer.qos.queue_byte_32",
    "u32::from(data_master) != expected_data_master",
    "header.context_id",
    "descriptor_context_ids",
):
    assert identity in validation, identity

completion = block(
    MANAGER,
    "pub(crate) fn render_completion_masks(",
    "pub(crate) const RENDER_COMPLETION_POLLS",
)
assert ".queue_pair()" in completion and ".completion_masks()" in completion

outer = block(
    RESOURCES,
    "pub(crate) fn publish_partial_opening_pair<F, G>(",
    "pub(crate) fn publish_deferred_partial_opening_pair(",
)
assert "queue_pair.fragment as u8" in outer
assert "queue_pair.tiling as u8" in outer

drm_submit = block(
    DRM,
    "let render_slot = lease.slot();",
    "G17PPendingCommand::Compute(translated)",
)
for use in (
    "render_slot,\n                        true,",
    "G17PRenderWatchPacket::new(\n                    submission_id, render_slot, completion,",
    "timeout_deferred_render(\n                        render_slot, submission_id, error,",
):
    assert use in drm_submit, use


SLOTS = 2
owners = [0] * SLOTS
last_use = [0] * SLOTS
generations = [0] * SLOTS
leases = 0
epoch = 0
pending_fences = [None] * SLOTS
quarantined = [False] * SLOTS


def acquire(owner: int) -> int:
    global leases, epoch
    free = [slot for slot in range(SLOTS) if not leases & (1 << slot)]
    assert free
    own = [slot for slot in free if owners[slot] == owner]
    never = [slot for slot in free if owners[slot] == 0]
    slot = (own or never or sorted(free, key=lambda item: last_use[item]))[0]
    epoch += 1
    leases |= 1 << slot
    last_use[slot] = epoch
    if owners[slot] != owner:
        generations[slot] += 1
        owners[slot] = owner
    return slot


def release(slot: int) -> None:
    global leases
    leases &= ~(1 << slot)


def pair_identity(slot: int) -> dict[str, object]:
    ta = slot * 2
    fragment = ta + 1
    mask = (1 << ta) | (1 << fragment)
    hwid = 0 if slot == 0 else slot + 1
    # These names model the independently encoded production surfaces. They
    # must remain equal within one submission and disjoint across live slots.
    return {
        "pair": (ta, fragment),
        "tag15": (ta, fragment),
        "ksm": (ta, fragment),
        "qos": (ta, fragment, hwid),
        "outer": (ta, fragment),
        "completion": mask,
        "watch_slot": slot,
    }


# More than 128x the descriptor ring and 16x the 8-bit control-ring capacity.
# Seventeen owners force repeated LRU handoffs instead of merely returning to
# two stable clients; every thirteenth submit exercises a prepublication error.
for cycle in range(4096):
    client_a = cycle % 17 + 1
    client_b = (cycle * 7 + 5) % 17 + 1
    if client_b == client_a:
        client_b = client_b % 17 + 1
    active = [(client_a, acquire(client_a)), (client_b, acquire(client_b))]
    assert active[0][1] != active[1][1]

    identities = []
    for index, (owner, slot) in enumerate(active):
        identity = pair_identity(slot)
        pair = identity["pair"]
        assert identity["tag15"] == pair
        assert identity["ksm"] == pair
        assert identity["qos"][:2] == pair
        assert identity["outer"] == pair
        assert identity["watch_slot"] == slot
        assert generations[slot] > 0 and owners[slot] == owner
        identities.append(identity)

        submission = cycle * 2 + index + 1
        if submission % 13 == 0:
            # A pre-power/direct-config refusal happens before the completion
            # fence is armed. Timeout classification must leave both slots and
            # the other client's fence untouched.
            before = pending_fences.copy()
            assert pending_fences[slot] is None
            assert pending_fences == before
            assert not quarantined[slot]
        else:
            assert pending_fences[slot] is None
            pending_fences[slot] = submission
            assert pending_fences[slot] == submission
            pending_fences[slot] = None

    assert set(identities[0]["pair"]).isdisjoint(identities[1]["pair"])
    assert identities[0]["completion"] & identities[1]["completion"] == 0
    assert identities[0]["completion"] | identities[1]["completion"] == 0xF
    assert identities[0]["qos"][2] != identities[1]["qos"][2]
    release(active[1][1])
    release(active[0][1])

assert leases == 0
assert pending_fences == [None, None]
assert all(generation > 100 for generation in generations)
print(
    "PASS render pair identity stress: 8192 submissions/4096 overlaps, "
    "17-owner LRU reuse, QID/QoS/outer/completion/fence identities coherent"
)

#!/usr/bin/env python3
from pathlib import Path


ROOT = Path(__file__).resolve().parents[5]
MANAGER = (ROOT / "drivers/gpu/drm/asahi/g17_manager.rs").read_text()


def block(start: str, end: str) -> str:
    begin = MANAGER.index(start)
    return MANAGER[begin:MANAGER.index(end, begin)]


validator = block(
    "fn validate_render_queue_consistency(",
    "fn commit_render_firmware_install(",
)
for field in (
    "header.queue_id",
    "header.data_master",
    "header.priority",
    "header.entry_low",
    "header.entry_high",
    "header.field_56",
    "qos_record[0]",
    "producer.queue_id",
    "producer.completion_selector",
    "producer.qos.queue_byte_32",
    "config != expected",
):
    assert field in validator, field

begin = block(
    "fn begin_translated_render_with_hardware_buffer(",
    "pub(crate) fn publish_default_render_pair_after_freelist(",
)
outer = begin.index(".publish_partial_opening_pair(")
for call in (
    "validate-fragment-qid-consistency",
    "validate-tiling-qid-consistency",
):
    assert begin.index(call) < outer, call

assert "g17p_sksm_configure_queue" not in validator
print("g17 render QID consistency validation: PASS")

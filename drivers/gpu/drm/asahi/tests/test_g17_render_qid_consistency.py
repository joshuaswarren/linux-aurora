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
    "descriptor_context_ids",
    "header.context_id",
    "parameter_buffer_binding",
    "header.parameter_buffer_object",
    "header.parameter_buffer_token",
    "header.parameter_buffer_id",
    "header.parameter_buffer_bit",
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

direct = block(
    "fn configure_fresh_secondary_render_pair(",
    "fn publish_render_sksm_group(",
)
for queue in ("fragment", "tiling"):
    assert f"queues.{queue}.config.write_pair()" in direct
    assert f"queues.{queue}.config.enable_pair" in direct
assert "g17p_sksm_configure_powered_queue" in direct
assert "if !fragment || !tiling" in direct
assert "return Err(EAGAIN);" in direct

direct_call = begin.index("self.configure_fresh_secondary_render_pair(")
assert direct_call < begin.index("let pm_submission =") < outer
assert "!execution_context.matches(vm) || !execution_context.is_current()" in MANAGER
print("g17 render QID consistency validation: PASS")

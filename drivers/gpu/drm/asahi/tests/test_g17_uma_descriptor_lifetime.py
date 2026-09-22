#!/usr/bin/env python3
"""Source guards for per-owner UMA descriptor publication and retention."""

from pathlib import Path


src = Path(__file__).resolve().parent.parent
resources = (src / "g17_resources.rs").read_text()
manager = (src / "g17_manager.rs").read_text()


def block(source: str, marker: str) -> str:
    start = source.index(marker)
    end = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


owner = block(resources, "pub(crate) struct G17PRenderFlist")
owner_impl = block(resources, "impl G17PRenderFlist")
constructor = block(resources, "    pub(crate) fn new_user_render_storage(")
alias = block(resources, "    fn stage_user_render_flist(")
stage = block(resources, "    fn stage_render_uma_owner(")
slot = block(manager, "struct G17PRenderSlotState")
swap = block(manager, "    fn swap_active_render_slot_state(")

assert "control_shared: MappedObject" in owner
assert "backing: MappedObject" in owner
assert "129u16.checked_add" in owner_impl
assert "put_u64(bytes, 0x14, page_list_va)" in owner_impl
assert "put_u64(bytes, 0x30, run_table_va)" in owner_impl
assert "put_u32(bytes, 0x08, u32::from(id))" in owner_impl

assert "G17PRenderFlist::new" in constructor
assert "self.stage_render_uma_owner(&storage.render_flist)?" not in constructor
assert "self.stage_render_flist_state()?" not in alias
assert "render_flist.map_user_backing(vm)?" in alias

assert "g17p_umadesc_offset(hardware_buffer_id)" in stage
assert "let state = owner.uma_state()?" in stage
assert "encode_g17p_umadesc(previous, state)" in stage

for field in ["bootstrap_uma", "prepared_render_flist", "next_render_flist_sequence"]:
    assert field in slot
    assert field in swap

begin = block(manager, "    fn begin_translated_render_with_hardware_buffer(")
assert "stage_user_render_uma" in begin
assert begin.index("stage_user_render_uma") < begin.index("G17PBootstrapUmaLifecycle::acquire_owner")

print("per-owner UMA descriptor lifetime/source guards passed")

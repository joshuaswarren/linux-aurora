#!/usr/bin/env python3
"""Source boundaries for job-private render roots and bounded slot switching."""
from pathlib import Path

root = Path(__file__).resolve().parents[1]
manager = (root / "g17_manager.rs").read_text()
runtime = (root / "g17_live_boot.rs").read_text()
drm = (root / "g17_drm.rs").read_text()
resources = (root / "g17_resources.rs").read_text()
mmu = (root / "mmu.rs").read_text()
pgtable = (root / "pgtable.rs").read_text()


def function(source, marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


handoff = function(manager, "pub(crate) fn handoff_retained_render_vm(")
assert handoff.index("self.require_retired_render_owner()?") < handoff.index("G17PRetiredRenderGraph {")
assert "self.render_bind.take().ok_or(EIO)?" in handoff
assert "self.render_context_aliases.take()" in handoff
assert "self.render_storage.take().ok_or(ENODEV)?" in handoff
assert "release_client_context" not in handoff
assert handoff.index("self.retired_render_handoff_graph.replace(retired)") < handoff.index(
    "Self::release_retired_handoff_graph("
)
assert "install_t8140_native_context_aliases" not in handoff
assert "incoming VM builds a new root" in handoff

retired_graph = manager[manager.index("struct G17PRetiredRenderGraph {"):]
retired_graph = retired_graph[:retired_graph.index("}\n")]
assert retired_graph.index("bind: G17PRenderRootOwner") < retired_graph.index(
    "context_aliases: Option<mmu::T8140NativeContextAliases>"
)
assert retired_graph.index("context_aliases: Option<mmu::T8140NativeContextAliases>") < retired_graph.index(
    "storage: g17_resources::G17PUserRenderStorage"
)
release_retired = function(manager, "fn release_retired_handoff_graph(")
assert release_retired.index("ids.unreserve(id)") < release_retired.index("drop(retired)")

retired = function(manager, "fn require_retired_render_owner(")
assert "snapshot.job_list_empty" in retired
assert "snapshot.tiling.idle()" in retired
assert "snapshot.fragment.idle()" in retired
assert "partial_opening_outer_counters" not in retired

take = function(manager, "fn take_active_render_slot_state(")
install = function(manager, "fn install_active_render_slot_state(")
for owner in (
    "render_storage", "render_tracker", "render_bind", "render_context_aliases",
    "retired_handoff_graph",
    "render_submission_ordinal", "render_auxiliary_timestamp",
    "render_hardware_buffer_id", "render_hardware_buffer_published", "render_sksm",
):
    assert owner in take
    assert owner in install

select = function(manager, "pub(crate) fn select_render_slot(")
assert "G17PRenderQueuePair::SLOT_COUNT" in select
assert "self.parked_render_slots[current_slot] = Some(current)" in select
assert ".unwrap_or_default()" in select

# The manager contains the active render owner and two complete parked state
# images. It must be initialized directly in its KBox: building `Self` by
# value grows the probe frame beyond the arm64 kernel stack once nested under
# boot_initial_session.
constructor = function(manager, "pub(crate) fn new_with_uat(")
assert "KBox::init(" in constructor
assert "try_init!(Self {" in constructor
assert "KBox::new(\n            Self {" not in constructor

outer = function(resources, "pub(crate) fn publish_partial_opening_pair<")
assert "queue_pair.fragment as u8" in outer
assert "queue_pair.tiling as u8" in outer
assert "g17p_render_queue_id(" not in outer

late_tiling = function(resources, "fn build_g17p_late_tiling_bytes(")
assert "queue_pair.tiling" in late_tiling
assert "queue_pair.fragment" in late_tiling
assert "queue_pair.qos_hardware_buffer_id()" in late_tiling
assert "g17p_render_queue_id(" not in late_tiling

stage_fragment = function(resources, "pub(crate) fn stage_fragment(")
assert "self.queue_pair.qos_hardware_buffer_id()" in stage_fragment
new_sksm = function(manager, "fn new_render_sksm_queues(")
assert "pair.qos_hardware_buffer_id()" in new_sksm
assert "G17P_NATIVE_RENDER_QOS_HARDWARE_BUFFER_ID" not in new_sksm

submit = function(runtime, "pub(crate) fn submit_translated_render(")
assert "select_render_slot(render_slot)?" in submit
assert "if defer_completion" in submit
watch = function(drm, "impl g17_job::G17PRenderWatchBackend for G17PRenderWatchBackend")
assert watch.count("packet.render_slot()") >= 4
assert watch.count("timeout_deferred_render(") == 2
assert "poll_deferred_render_completion" not in watch
# Ordinary completion is event-driven with the runtime mutex released. Only
# scheduler timeout/cancel enters the bounded final-observation path above.
events = function(runtime, "fn service_async_render_completions(")
assert "self.poll_deferred_render_completion(slot)?" in events
assert "signal.complete(Ok(observed))" in events

# Job-root SecureGart setup must retain Mesa's six existing GPUVM low views.
# Only the VM-lifetime-reserved null view is a driver-owned KernelMapping.
job_aliases = function(mmu, "pub(crate) fn install_t8140_job_context_aliases(")
assert ".enumerate().skip(1)" in job_aliases
assert "T8140_CLIENT_NULL_VIEW" in job_aliases
assert job_aliases.count("source.map_in_range(") == 1
assert "source.translate_iova(high)? != source.translate_iova(low)?" in job_aliases

bind_object = function(mmu, "pub(crate) fn bind_object(")
assert ".alloc_pages(" not in bind_object

# The authoritative PTE layer rejects cross-allocator collisions instead of
# logging and overwriting one owner with another.
prepare = function(pgtable, "pub(crate) fn prepare_map(")
map_pages = function(pgtable, "pub(crate) fn map_pages(")
assert "return Err(EBUSY)" in prepare
assert "return Err(EBUSY)" in map_pages
assert "pte.store(phys" in map_pages
assert map_pages.index("return Err(EBUSY)") < map_pages.index("pte.store(phys")
unmap_pages = function(pgtable, "pub(crate) fn unmap_pages(")
assert "if iova_range.is_empty()" in unmap_pages
assert unmap_pages.index("if iova_range.is_empty()") < unmap_pages.index(".covers_range(")

# Process-root queue peers occupy the bounded gap between the render and
# relocated-compute FLists, while context-0 keeps the native queue addresses.
low_va = function(resources, "const fn low_va(")
assert "G17P_RENDER_JOB_QUEUE_LOW_BASE" in low_va
assert "G17P_RENDER_QUEUE_LOW_BASE" not in low_va
assert "G17P_RENDER_JOB_QUEUE_LOW_BASE == 0x70_01ef_8000" in resources

storage_impl = resources[resources.index("impl G17PUserRenderStorage {"):]
storage_new = function(storage_impl, "fn new(")
assert storage_new.count("g17p_cl_client_low_va_range()") >= 3
assert "G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA\n                    .." not in storage_new
reachability = function(resources, "pub(crate) fn first_unreachable(")
assert '"primary-index-page"' not in reachability
assert '"pm-page-list"' in reachability
assert "pm.page_list_low_va()" in reachability
assert "g17p_render_page_list_va(self.queue_pair)" in reachability
ta_status = reachability[reachability.index('"ta-status-page"'):]
ta_status = ta_status[:ta_status.index('"3d-status-page"')]
assert "vm.covers_range(" in ta_status
assert "uat.kernel_vm().covers_range(" not in ta_status

print("PASS job-private VM handoff and complete two-slot owner parking; ordinary completion is event-driven")

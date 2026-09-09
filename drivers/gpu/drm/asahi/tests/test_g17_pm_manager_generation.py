#!/usr/bin/env python3
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MANAGER = (ROOT / "g17_manager.rs").read_text()
RESOURCES = (ROOT / "g17_resources.rs").read_text()


def body(source: str, start: str, end: str) -> str:
    return source[source.index(start):source.index(end, source.index(start))]


assert "current.checked_add(1).ok_or(EOVERFLOW)?" in RESOURCES
assert "if manager_generation == 0" in RESOURCES
assert "put_u64(raw, selected_scene + 0x4c, manager_generation)" in RESOURCES
assert "manager_generation: G17P_PM_INITIAL_HWPB_MANAGER_GENERATION" not in RESOURCES

bootstrap = body(
    MANAGER,
    "pub(crate) fn stage_compute_bootstrap_render(",
    "pub(crate) fn publish_primary_device_control_record(",
)
assert bootstrap.index("prepare_render_pm_manager_generation()?") < bootstrap.index(
    ".new_user_render_storage("
)
assert bootstrap.index("self.render_storage = Some(") < bootstrap.index(
    "commit_render_pm_manager_generation(pm_generation)?"
)

submit = body(
    MANAGER,
    "fn begin_translated_render_with_hardware_buffer(",
    "pub(crate) fn publish_default_render_pair_after_freelist(",
)
assert submit.index("prepare_render_pm_manager_generation()?") < submit.index(
    ".new_user_render_storage("
)
assert submit.index("self.render_storage = Some(match storage") < submit.index(
    "commit_render_pm_manager_generation(pm_generation)"
)

commit = body(
    MANAGER,
    "fn commit_render_pm_manager_generation(",
    "/// Validate the incoming retained storage's indexed PBDesc identity.",
)
assert "current == actual && current == self.next_render_pm_manager_generation" in commit
assert commit.index("current == actual") < commit.index(
    "self.next_render_pm_manager_generation = next"
)

stopped = body(
    MANAGER,
    "pub(crate) fn release_runtime_objects_after_processor_stop(",
    "pub(crate) fn reset_session_after_processor_stop(",
)
assert "next_render_pm_manager_generation" in stopped

print("g17 PM manager generation lifecycle: PASS")

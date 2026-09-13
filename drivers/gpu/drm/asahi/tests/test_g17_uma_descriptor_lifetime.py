#!/usr/bin/env python3
"""Compile the real UMA staging path against byte-backed mappings."""

from pathlib import Path
import subprocess
import tempfile


src = Path(__file__).resolve().parent.parent
resources = (src / "g17_resources.rs").read_text()
submission = (src / "g17_submission.rs").read_text()


def block(source, marker):
    start = source.index(marker)
    end = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


constructor = block(resources, "    pub(crate) fn new_user_render_storage(")
pool = block(resources, "    pub(crate) fn stage_render_flist_state(")
uma = block(resources, "    pub(crate) fn stage_render_uma_page_pool_descriptor(")
alias = block(resources, "fn stage_user_render_flist(")
call = "self.stage_render_uma_page_pool_descriptor()?;"
assert "self.stage_render_flist_state()?" in alias
assert call not in constructor
assert pool.index(call) < pool.index("Ordering::SeqCst")

wire = "\n".join(
    [
        "pub const G17P_UMA_DESC_ENTRY_SIZE: usize = 0x20;",
        "pub const G17P_UMA_DESC_ENTRY_COUNT: u32 = 0x100;",
        "#[derive(Default)] "
        + block(submission, "pub(crate) struct G17PUmaPagePoolState"),
        block(submission, "pub(crate) const fn encode_g17p_umadesc("),
        block(submission, "pub(crate) const fn g17p_umadesc_offset("),
    ]
)

body = (
    r'''
#![allow(dead_code, unused_assignments)]
type Result<T = ()> = std::result::Result<T, i32>;
const EINVAL: i32 = 22;
macro_rules! dev_info { ($($arg:tt)*) => {} }
#[derive(Clone, Debug, PartialEq, Eq)] struct Bytes(Vec<u8>);
impl Bytes {
    fn with_bytes_mut<T>(&mut self, f: impl FnOnce(&mut [u8]) -> Result<T>) -> Result<T> {
        f(&mut self.0)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)] struct Backings {
    control_operand_page_lists: Bytes, control_operand_table: Bytes,
    control_shared: Bytes, control_shared_inner: Bytes,
    uma_page_pool_descriptor_table: Bytes,
}
#[derive(PartialEq, Eq)] enum G17PFListBackingOwner { Opening, Render }
struct Owner { resources: Backings, flist_backing_owner: G17PFListBackingOwner }
fn encode_g17p_initial_flist_page_list(b: &mut [u8]) -> bool { b.fill(0x17); true }
fn encode_g17p_pre_qid_flist_runs(b: &mut [u8]) -> bool { b.fill(0x22); true }
mod g17_initdata {
    pub const PARTIAL_OPENING_FREELIST_HARDWARE_BUFFER_ID: u32 = 0;
    pub const CONTROL_SHARED_INNER_PRESENTED: u64 = 1;
    pub fn encode_render_control_shared(b: &mut [u8]) -> Result<(), ()> {
        b.fill(0);
        b[0x14..0x1c].copy_from_slice(&0x7000000000u64.to_le_bytes());
        b[0x1c..0x20].copy_from_slice(&0x40000u32.to_le_bytes());
        b[0x24..0x28].copy_from_slice(&0x1100u32.to_le_bytes());
        b[0x2c..0x30].copy_from_slice(&0x1100u32.to_le_bytes());
        Ok(())
    }
}
'''
    + "\nmod g17_submission {\n"
    + wire
    + "\n}\nimpl Owner {\n"
    + pool
    + uma
    + r'''
fn new_client_pool_stage(&mut self) -> Result {
    self.stage_render_flist_state()
}
}
fn owner() -> Owner {
    let b = Bytes(vec![0xa5; 0x4000]);
    Owner { resources: Backings {
        control_operand_page_lists: b.clone(), control_operand_table: b.clone(),
        control_shared: b.clone(), control_shared_inner: b.clone(),
        uma_page_pool_descriptor_table: b,
    }, flist_backing_owner: G17PFListBackingOwner::Opening }
}
#[test]
fn second_client_preserves_firmware_uma_cursors() {
    let mut o = owner();
    o.new_client_pool_stage().unwrap();
    let table = &mut o.resources.uma_page_pool_descriptor_table.0;
    table[8..16].copy_from_slice(&((0x123u64 << 5) | (0x1500u64 << 33)).to_le_bytes());
    table[16..24].copy_from_slice(&0x1600u64.to_le_bytes());
    let live = o.resources.clone();
    for _ in 0..64 {
        o.new_client_pool_stage().unwrap();
        assert!(o.resources == live, "new client replayed cold pool state");
    }
}
#[test]
fn cold_and_stopped_session_initialize_once() {
    let mut o = owner();
    o.new_client_pool_stage().unwrap();
    let cold = o.resources.clone();
    assert_eq!(&cold.uma_page_pool_descriptor_table.0[16..24], &0x1100u64.to_le_bytes());
    o.resources.uma_page_pool_descriptor_table.0[8..24].fill(0x55);
    o.flist_backing_owner = G17PFListBackingOwner::Opening;
    o.new_client_pool_stage().unwrap();
    assert!(o.resources == cold, "stopped-session initialization differs");
}
#[test]
fn failed_publication_does_not_claim_initialized_pool() {
    let mut o = owner();
    o.resources.uma_page_pool_descriptor_table.0.truncate(16);
    assert_eq!(o.new_client_pool_stage(), Err(EINVAL));
    assert!(o.flist_backing_owner == G17PFListBackingOwner::Opening);
    o.resources.uma_page_pool_descriptor_table.0.resize(0x4000, 0xa5);
    o.new_client_pool_stage().unwrap();
    assert!(o.flist_backing_owner == G17PFListBackingOwner::Render);
}
'''
)

with tempfile.TemporaryDirectory(prefix="g17-uma-lifetime-") as temp:
    binary = Path(temp) / "tests"
    subprocess.run(
        ["rustc", "--edition=2021", "--test", "-", "-o", str(binary)],
        input=body,
        text=True,
        check=True,
    )
    subprocess.run([str(binary)], check=True)

print("G17P UMA descriptor cold-publish and warm-client lifetime checks PASS")

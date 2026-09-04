#!/usr/bin/env python3
"""Compile actual RCE address selection/encoding and retained self-pointer code."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
resources = (root / "g17_resources.rs").read_text()
submission = (root / "g17_submission.rs").read_text()
render = (root / "g17_render.rs").read_text()
manager = (root / "g17_manager.rs").read_text()


def function(source, marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


getter = function(resources, "pub(crate) fn descriptor_low_va(")
encoder = function(submission, "pub(crate) fn prepare_g17p_render_cl_command_state(")
mcache_encoder = function(submission, "fn encode_g17p_cl_mcache(")
retained = function(render, "pub(crate) fn apply_g17p_retained_descriptor_fields(")
assert "self.descriptor_3d_low.iova()" in getter
assert "descriptor_3d_render" not in getter
assert resources.count("g17_render::RenderDescriptorKind::Fragment,\n                descriptor_3d_rce_va,") == 2
assert "let descriptor_3d_rce_va = descriptor_3d_low.iova();" in resources
assert "let descriptor_3d_rce_va = self.descriptor_3d_low.iova();" in resources
assert ".audit_fragment_rce_mapping(dev, self.resources.uat().kernel_lower_vm())" in manager
audit = function(resources, "pub(crate) fn audit_fragment_rce_mapping(")
assert "let base = self.descriptor_3d_low.iova();" in audit
assert "rce_vm.read_bytes(address" in audit
assert "return Ok(())" not in audit  # Validation cannot be hidden behind a dump flag.
assert resources.count("descriptor_3d_rce_va,\n                g17_render::G17P_FRAGMENT_MCACHE_OFFSET,") == 2

unit = r'''
#[derive(Debug)] enum SubmissionError { ComputeRceAddressOverflow, KickMcacheFieldOutOfRange, KickMcacheAddressUnencodable }
#[derive(Debug)] enum RenderBuildError { ArithmeticOverflow, BufferTooSmall }
enum RenderDescriptorKind { Tiling, Fragment }
const TA_DESCRIPTOR_SIZE: usize = 0x9c0;
const FRAGMENT_DESCRIPTOR_SIZE: usize = 0x2240;
const G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA: u64 = 0x7000000000;
#[derive(Clone,Copy,Debug,PartialEq)] struct G17PClRceBinding { address: u64, tag: u8 }
struct G17PComputeClCommandState { event_mask: [u64;4], rce_bindings: [G17PClRceBinding;4] }
struct G17PClMcacheAperture { address: u64, index: u8, count: u8 }
const G17P_CL_PACKED_ADDRESS_MASK: u64 = (1u64 << 38) - 1;
struct Mapping(u64);
impl Mapping { fn iova(&self) -> u64 { self.0 } }
struct Storage { descriptor_3d_low: Mapping, descriptor_3d_render: Mapping }
mod module_parameters {
    pub struct Param(u32);
    impl Param { pub fn value(&self) -> &u32 { &self.0 } }
    pub static g17p_render_3d_rce: Param = Param(4);
    pub static g17p_render_ta_qid_field: Param = Param(1);
}
mod g17_submission { pub const G17P_NATIVE_RENDER_DOORBELL_PRIORITY: u32 = 0; }
fn put_u8(raw: &mut [u8], at: usize, v: u8) { raw[at] = v; }
fn put_u32(raw: &mut [u8], at: usize, v: u32) { raw[at..at+4].copy_from_slice(&v.to_le_bytes()); }
fn put_u64(raw: &mut [u8], at: usize, v: u64) { raw[at..at+8].copy_from_slice(&v.to_le_bytes()); }
#[test]
fn ksm_fetches_use_existing_global_alias_not_client_allocation() {
    let storage = Storage { descriptor_3d_low: Mapping(0x7000098000),
                            descriptor_3d_render: Mapping(0x700203c000) };
    let command = prepare_g17p_render_cl_command_state(storage.descriptor_low_va(false), false).unwrap();
    assert_eq!(command.rce_bindings.map(|b|b.address),
               [0x70000980a0, 0x70000987c0, 0x7000098ee0, 0x7000099600]);
    assert_eq!(storage.descriptor_low_va(true), 0x7000000000);
}
#[test]
fn cold_and_retained_self_trailers_use_the_same_context0_identity() {
    let storage = Storage { descriptor_3d_low: Mapping(0x7000098000),
                            descriptor_3d_render: Mapping(0x700203c000) };
    for ordinal in [0,1,7] {
        let mut raw = [0u8; FRAGMENT_DESCRIPTOR_SIZE];
        apply_g17p_retained_descriptor_fields(RenderDescriptorKind::Fragment,
            storage.descriptor_low_va(false), 0, 1, 1, ordinal, ordinal==0, false, &mut raw).unwrap();
        for (at, expected) in [(0x7a0,0x70000980a0u64), (0xec0,0x70000987c0),
                               (0x15e0,0x7000098ee0), (0x1d00,0x7000099600)] {
            assert_eq!(u64::from_le_bytes(raw[at..at+8].try_into().unwrap()), expected);
        }
    }
}
#[test]
fn mcache_table_pointer_matches_native_global_descriptor_encoding() {
    let storage = Storage { descriptor_3d_low: Mapping(0x7000098000),
                            descriptor_3d_render: Mapping(0x700203c000) };
    let aperture = G17PClMcacheAperture {
        address: storage.descriptor_low_va(false) + 0x6a0, index: 0, count: 2,
    };
    assert_eq!(encode_g17p_cl_mcache(aperture).unwrap(), 0x004000e000130d40);
}
'''
with tempfile.TemporaryDirectory(prefix="g17-rce-context0-") as tmp:
    test = Path(tmp) / "test.rs"
    test.write_text(unit + "\nimpl Storage {\n" + getter + "\n}\n" + encoder + mcache_encoder + retained)
    exe = Path(tmp) / "test"
    subprocess.run(["rustc", "-Awarnings", "--edition=2021", "--test", str(test), "-o", str(exe)], check=True)
    subprocess.run([str(exe)], check=True)
print("RCE context0 source integration checks passed")

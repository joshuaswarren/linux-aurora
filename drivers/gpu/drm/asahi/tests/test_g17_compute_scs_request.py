#!/usr/bin/env python3
"""The per-kick ray-traversal (SCS) scratch request must reach three registers.

AGXMetalG17P sizes a compute pass's shader-context-state section and the kext's
register builder (generateCLRegisterListG16Common) programs that request as
0x101d1 = pages | MiB << 16, 0xd479 = extras, and 0x107a1 = window << 16 |
0x0100_0000 whenever any page is requested, falling back to 0x00ff_0000 when
none is. The connection between the previously unconditional zero-page form
and zero-valued SCS readbacks remains a hardware-validation question.

This test pins both halves: an empty request still reproduces the historical
constants byte for byte, and a real request packs the fields the way the kext
does.
"""
from pathlib import Path
import json
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
compute = (root / "g17_compute.rs").read_text()
params = (root / "asahi.rs").read_text()


def block(text, marker):
    a = text.index(marker)
    i = text.index("{", a) + 1
    depth = 1
    while depth:
        depth += (text[i] == "{") - (text[i] == "}")
        i += 1
    return text[a:i]


# The register program must consume the request, not the old constants.
program = block(compute, "pub(crate) fn build_compute_registers(")
assert "ComputeRegister::new(0x101d1, scs.size_word)" in program
assert "ComputeRegister::new(0x0d479, scs.extras)" in program
assert "ComputeRegister::new(0x107a1, scs.window_word)" in program
assert "0x101d1, 0)" not in program
assert "0x107a1, 0xff_0000)" not in program

# Every knob is opt-in: a default build must be byte-identical to before.
for name in (
    "g17p_compute_scs_pages",
    "g17p_compute_scs_mib",
    "g17p_compute_scs_extras",
    "g17p_compute_scs_window",
):
    declaration = block(params, f"        {name}: u32 {{")
    assert "default: 0," in declaration, f"{name} must default to 0"

# Compile the actual complete module, including its historical 40-register
# hash test. Stub only module-parameter access, using the real defaults.
declarations = []
for name in sorted(set(re.findall(r"crate::module_parameters::(\w+)\.value", compute))):
    declaration = block(params, f"        {name}: u32 {{")
    default = re.search(r"default:\s*(0x[0-9a-fA-F_]+|[0-9_]+)\s*,", declaration)
    assert default, f"cannot resolve the real default for {name}"
    declarations.append(f"pub static {name}: Param = Param({default.group(1)});")

rust = "#![allow(dead_code, non_upper_case_globals)]\n"
rust += "mod module_parameters {\npub struct Param(u32);\n"
rust += "impl Param { pub fn value(&self) -> &u32 { &self.0 } }\n"
rust += "\n".join(declarations) + "\n}\n"
rust += f"#[path = {json.dumps(str(root / 'g17_compute.rs'))}] mod g17_compute;\n"
rust += "use g17_compute::*;\n"
rust += r'''
#[test] fn an_empty_request_is_the_historical_register_program() {
 let scs = scs_request_registers(0, 0, 0, 0).unwrap();
 assert_eq!(scs.size_word, 0);
 assert_eq!(scs.extras, 0);
 assert_eq!(scs.window_word, 0xff_0000);
}
#[test] fn all_overrides_are_ignored_while_no_pages_are_requested() {
 assert_eq!(scs_request_registers(0, u32::MAX, u32::MAX, u32::MAX),
            scs_request_registers(0, 0, 0, 0));
}
#[test] fn a_request_packs_pages_and_mib_into_one_register() {
 // 89 pages is the four-section floor (0x58000) plus Apple's guard page, and
 // one MiB covers it: (89 * 4 + 0x3ff) >> 10 == 1.
 let scs = scs_request_registers(89, 1, 0, 0).unwrap();
 assert_eq!(scs.size_word, 0x0001_0059);
 assert_eq!(scs.window_word, 0x0100_0000);
}
#[test] fn extras_and_window_take_their_own_places() {
 let scs = scs_request_registers(815, 4, 2, 0x20).unwrap();
 assert_eq!(scs.size_word, 0x0004_032f);
 assert_eq!(scs.extras, 2);
 assert_eq!(scs.window_word, 0x0120_0000);
}
#[test] fn fields_cannot_overlap_or_silently_truncate() {
 for (pages, mib, window) in [(0x10000, 0, 0), (1, 0x10000, 0), (1, 0, 0x100)] {
  assert_eq!(scs_request_registers(pages, mib, 0, window),
             Err(ComputeBuildError::ScsRequestOutOfRange));
 }
 let max = scs_request_registers(0xffff, 0xffff, u32::MAX, 0xff).unwrap();
 assert_eq!(max.size_word, 0xffff_ffff);
 assert_eq!(max.extras, u32::MAX as u64);
 assert_eq!(max.window_word, 0x01ff_0000);
}
#[test] fn derived_mib_rounds_up_at_every_page_boundary() {
 for pages in 1..=0xffff {
  let scs = scs_request_registers(pages, 0, 0, 0).unwrap();
  assert_eq!(scs.size_word & 0xffff, pages as u64);
  assert_eq!(scs.size_word >> 16, ((pages as u64 * 4096) + 0xfffff) >> 20);
 }
}
'''

with tempfile.TemporaryDirectory() as d:
    source = Path(d) / "test.rs"
    source.write_text(rust)
    exe = Path(d) / "test"
    subprocess.run(["rustc", "--edition=2021", "--test", str(source), "-o", str(exe)], check=True)
    subprocess.run([str(exe), "--test-threads=1"], check=True)

print("PASS the SCS scratch request reaches 0x101d1/0xd479/0x107a1 and defaults to off")

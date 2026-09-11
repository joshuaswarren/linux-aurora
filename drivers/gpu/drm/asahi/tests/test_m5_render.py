#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Compile the real M5 register codec and run its native-reference tests."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = r'''
#![allow(dead_code, non_upper_case_globals)]
mod module_parameters {
    pub struct Param(u32);
    impl Param { pub fn value(&self) -> &u32 { &self.0 } }
    pub static g17p_ta_oracle: Param = Param(0);
    pub static g17p_frag_oracle: Param = Param(0);
    pub static g17p_render_ta_qid_field: Param = Param(1);
}
mod g17_submission {
    pub const G17P_NATIVE_RENDER_DOORBELL_PRIORITY: u32 = 0;
}
'''
with tempfile.TemporaryDirectory(prefix='m5-render-') as tmp:
    test = Path(tmp) / 'test.rs'
    test.write_text(source + ''.join(
        '#[path = "' + str(root / (module + '.rs')) + '"] mod ' + module + ';\n'
        for module in ('g17_render', 'm5_render')))
    exe = Path(tmp) / 'test'
    subprocess.run(['rustc', '-Awarnings', '--edition=2021', '--test', str(test),
                    '-o', str(exe)], check=True)
    listing = subprocess.check_output([str(exe), '--list'], text=True)
    for required in ('native_m5_register_lists_match_every_write',
                     'native_m5_partial_programs_match_every_write'):
        assert 'm5_render::tests::' + required + ': test' in listing
    subprocess.run([str(exe), 'm5_render::'], check=True)

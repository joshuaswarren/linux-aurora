#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Compare the controlled Rust program with private native M5 captures.

Does not qualify Linux execution or prove that these pages exhaust every
firmware-owned dependency of a command.
"""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import tempfile


def source_path():
    for parent in Path(__file__).resolve().parents:
        for root in (parent, parent / 'linux'):
            path = root / 'drivers/gpu/drm/asahi/m5_compute_program.rs'
            if path.is_file():
                return path
    raise RuntimeError('Cannot locate production M5 compute program')


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('captures', type=Path, nargs='+')
args = parser.parse_args()
with tempfile.TemporaryDirectory(prefix='m5-program-') as temporary:
    temporary = Path(temporary)
    harness = temporary / 'main.rs'
    harness.write_text('''use std::io::{Read, Write};
#[path = %s] mod program;
use program::*;
fn main() {
    if std::env::args().nth(1).as_deref() == Some("check") {
        let mut data = Vec::new();
        std::io::stdin().read_to_end(&mut data).unwrap();
        let r = check_result(&data).unwrap();
        println!("{} {} {}", r.inputs_changed, r.outputs_wrong, r.outputs_untouched);
        return;
    }
    let mut wrong = [0x55u8; 16];
    assert!(initialize_page(PageKind::Shader, &mut wrong).is_err());
    assert_eq!(wrong, [0x55; 16]);
    assert!(check_result(&wrong).is_err());
    let mut out = std::io::stdout().lock();
    for address in [SHADER_PAGE, PIPELINE_PAGE, STREAM_PAGE, PREEMPT_PAGE, BUFFER_PAGE, ROBUSTNESS_PAGE] {
        out.write_all(&address.to_le_bytes()).unwrap();
    }
    for kind in [PageKind::Shader, PageKind::Pipeline, PageKind::Stream,
                 PageKind::Preempt, PageKind::Buffers, PageKind::Robustness] {
        let mut page = [0u8; PAGE_BYTES];
        initialize_page(kind, &mut page).unwrap();
        out.write_all(&page).unwrap();
    }
}
''' % json.dumps(str(source_path())))
    executable = temporary / 'verify'
    subprocess.run(['rustc', '--edition=2021', '-Awarnings', str(harness), '-o', str(executable)], check=True)
    generated = subprocess.check_output([str(executable)])
    assert len(generated) == 48 + 6 * 0x4000
    addresses = struct.unpack_from('<6Q', generated)
    pages = [generated[48 + i * 0x4000:48 + (i + 1) * 0x4000] for i in range(6)]
    labels = ['compute-extra-0', 'compute-usc', 'compute-cdm',
              'compute-preempt', 'compute-extra-1', 'compute-robustness']
    ranges = [(0, 0x400), (0, 132), (0, 48), (0, 0x4000),
              (0xd00, 0x2d00), (0, 0x4000)]
    assert subprocess.check_output([str(executable), 'check'], input=pages[4]).strip() == b'0 1024 1024'
    completed = 0
    mapping_profiles = 0
    for directory in args.captures:
        manifest = json.loads((directory / 'manifest.json').read_text())
        assert manifest['compute_context'] == 3
        objects = {item['label']: item for item in manifest['objects']}
        has_mappings = any('mapping' in objects[label] for label in labels)
        for index, (label, (start, end)) in enumerate(zip(labels, ranges)):
            item = objects[label]
            assert item['stable'] and int(item['address'], 16) == addresses[index], label
            native = (directory / item['file']).read_bytes()
            assert pages[index][start:end] == native[start:end], (directory.name, label)
            if has_mappings:
                assert item.get('mapping_stable') is True, (directory.name, label, 'unstable mapping')
                # Context3 roots/tables match; shader and CDM are GPU-RO,
                # while pipeline, preemption, buffers and robustness are RW.
                expected_leaf = 0x80000000000c8b if index in (0, 2) else 0xc0000000000c8b
                physical_mask = ((1 << 42) - 1) & ~0x3fff
                assert item['mapping'], label
                for view in item['mapping']:
                    attributes = [int(entry, 16) & ~physical_mask for _, entry in view['walk']]
                    assert attributes == [0x3000000000001, 3, 3, expected_leaf], (directory.name, label, attributes)
        mapping_profiles += int(has_mappings)
        if manifest.get('completion', {}).get('same_descriptor'):
            item = objects['completed-extra-1']
            assert item['stable']
            native = (directory / item['file']).read_bytes()
            assert subprocess.check_output([str(executable), 'check'], input=native).strip() == b'0 0 0'
            # Reject an otherwise successful image if input was overwritten.
            changed = bytearray(native)
            changed[0xd00] ^= 1
            assert subprocess.check_output([str(executable), 'check'], input=changed).strip() == b'1 0 0'
            completed += 1
    print(json.dumps(dict(captures_matched=len(args.captures), completed_results_matched=completed,
                         mapping_profiles_matched=mapping_profiles,
                         pages_per_capture=6, shader_bytes=1024, pipeline_bytes=132,
                         cdm_bytes=48, linux_execution_qualified=False)))

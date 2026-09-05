#!/usr/bin/env python3
"""Exercise actual retained-render ownership methods without hardware."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
manager = (root / 'g17_manager.rs').read_text()
runtime = (root / 'g17_live_boot.rs').read_text()

def function(source, marker):
    start = source.index(marker)
    brace = source.index('{', start)
    end, depth = brace + 1, 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

methods = '\n'.join(function(manager, marker) for marker in [
    'pub(crate) fn retained_render_has_other_owner(',
    'pub(crate) fn retained_render_needs_recycle(',
])
check = function(runtime, 'fn check_retained_render_owner(')
for marker in ['pub(crate) fn submit_translated_render(',
               'pub(crate) fn submit_translated_compute(']:
    body = function(runtime, marker)
    guard = body.index('self.check_retained_render_owner(vm)?;')
    assert guard < body.index('recover_persistent_runtime(self)?;')
    if 'self.manager.as_mut()' in body:
        assert guard < body.index('self.manager.as_mut()')
    assert guard < body.index('self.settle_open_recovery_before_publication()?;')
    assert body[:guard].count('self.') <= 3, 'Admission must precede runtime mutations'

source = r'''
#![allow(dead_code)]
type Result<T = ()> = core::result::Result<T, i32>;
const ENODEV: i32 = 19;
const ENOTSUPP: i32 = 524;
mod mmu { #[derive(Clone, Copy)] pub struct Vm(pub u64); }
#[derive(Clone, Copy)]
struct Binding(u64);
impl Binding { fn matches(&self, vm: &mmu::Vm) -> bool { self.0 == vm.0 } }
struct Manager {
    render_storage: Option<()>,
    render_bind: Option<Binding>,
    render_context_aliases: Option<Binding>,
    render_submission_ordinal: u32,
}
struct Runtime { manager: Option<Manager>, accepting_submissions: bool }
'''
source += '#[path = "' + str(root / 'g17_lifecycle.rs') + '"] mod g17_lifecycle;\n'
source += 'impl Manager {\n' + methods + '\n}\n'
source += 'impl Runtime {\n' + check + '\n}\n'
source += r'''
#[test]
fn ownership_matrix_rejects_any_retained_owner_mismatch() {
    for storage in [false, true] {
        for bind in [None, Some(Binding(1)), Some(Binding(2))] {
            for aliases in [None, Some(Binding(1)), Some(Binding(2))] {
                let manager = Manager { render_storage: storage.then_some(()),
                    render_bind: bind, render_context_aliases: aliases,
                    render_submission_ordinal: 123 };
                let expected_conflict = (storage && bind.map(|b| b.0) != Some(1))
                    || aliases.is_some_and(|a| a.0 != 1);
                let runtime = Runtime { manager: Some(manager), accepting_submissions: true };
                assert_eq!(runtime.check_retained_render_owner(&mmu::Vm(1)),
                           if expected_conflict { Err(ENOTSUPP) } else { Ok(()) });
                let after = runtime.manager.as_ref().unwrap();
                assert_eq!(after.render_storage.is_some(), storage);
                assert_eq!(after.render_bind.map(|b| b.0), bind.map(|b| b.0));
                assert_eq!(after.render_context_aliases.map(|b| b.0), aliases.map(|b| b.0));
                assert_eq!(after.render_submission_ordinal, 123);
                assert!(runtime.accepting_submissions);
            }
        }
    }
}
#[test]
fn rejected_new_vm_preserves_the_original_vm_and_ordinal_guard() {
    let mut runtime = Runtime { manager: Some(Manager {
        render_storage: Some(()), render_bind: Some(Binding(7)),
        render_context_aliases: Some(Binding(7)), render_submission_ordinal: 256,
    }), accepting_submissions: true };
    assert_eq!(runtime.check_retained_render_owner(&mmu::Vm(8)), Err(ENOTSUPP));
    assert_eq!(runtime.check_retained_render_owner(&mmu::Vm(7)), Ok(()));
    assert!(!runtime.manager.as_ref().unwrap().retained_render_needs_recycle(&mmu::Vm(7)));
    runtime.manager.as_mut().unwrap().render_submission_ordinal = 0x00ff_ffff;
    assert!(runtime.manager.as_ref().unwrap().retained_render_needs_recycle(&mmu::Vm(7)));
}
#[test]
fn missing_manager_fails_closed() {
    let runtime = Runtime { manager: None, accepting_submissions: true };
    assert_eq!(runtime.check_retained_render_owner(&mmu::Vm(1)), Err(ENODEV));
}
'''
with tempfile.TemporaryDirectory() as temp:
    path = Path(temp) / 'owner.rs'
    path.write_text(source)
    exe = Path(temp) / 'owner-tests'
    subprocess.run(['rustc', '--edition=2021', '--test', str(path), '-o', str(exe)], check=True)
    for test in ['ownership_matrix_', 'rejected_new_vm_', 'missing_manager_']:
        subprocess.run([str(exe), test, '--nocapture'], check=True)
print('PASS: actual manager/runtime ownership methods and both submission admission orderings')

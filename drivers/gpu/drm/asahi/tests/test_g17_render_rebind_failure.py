#!/usr/bin/env python3
"""Fault-inject every fallible step in the actual cached-graph transaction."""
from pathlib import Path
import subprocess
import tempfile

text = (Path(__file__).resolve().parents[1] / "g17_manager.rs").read_text()
start = text.index("fn reactivate_recycled_render_graph(")
end = text.index("{", start) + 1
depth = 1
while depth:
    depth += (text[end] == "{") - (text[end] == "}")
    end += 1

source = r'''
#![allow(dead_code, unused_variables)]
use std::{sync::Arc, rc::Rc, cell::RefCell};
type Result<T = ()> = std::result::Result<T, i32>;
const EIO: i32 = 5;
macro_rules! dev_info { ($($arg:tt)*) => {}; }
macro_rules! dev_err { ($($arg:tt)*) => {}; }
#[derive(Default)]
struct Trace { fail: u8, dropped_storage: usize, dropped_roots: Vec<u64> }
type Shared = Rc<RefCell<Trace>>;
mod mmu {
    use super::*;
    pub struct Vm(pub u64);
    impl Vm {
        pub fn id(&self) -> u64 { self.0 }
        pub fn clear_render_pool_mappings(&self, _: u8) {}
    }
    pub struct T8140ComputeExecutionContext { pub vm: Vm, pub trace: Shared }
    impl Drop for T8140ComputeExecutionContext {
        fn drop(&mut self) { self.trace.borrow_mut().dropped_roots.push(self.vm.id()); }
    }
}
enum G17PRenderRootOwner { Job { context: Arc<mmu::T8140ComputeExecutionContext> } }
impl G17PRenderRootOwner {
    fn vm(&self) -> &mmu::Vm { let Self::Job { context } = self; &context.vm }
    fn vm_id(&self) -> u64 { self.vm().id() }
}
struct Pair;
impl Pair { fn slot(&self) -> u8 { 0 } }
struct Storage { trace: Shared, context: Option<Arc<mmu::T8140ComputeExecutionContext>>, backing: Option<()> }
impl Storage {
    fn check(&self, step: u8) -> Result { if self.trace.borrow().fail == step { Err(EIO) } else { Ok(()) } }
    fn queue_pair(&self) -> Pair { Pair }
    fn retarget_command_context(&mut self, _: &mmu::Vm, ctx: Arc<mmu::T8140ComputeExecutionContext>) -> Result {
        self.check(1)?; self.context = Some(ctx); Ok(())
    }
    fn prepare_cached_reactivation(&mut self, _: u32) -> Result { self.check(3) }
    fn claim_sksm_backing(&mut self, backing: &mut Option<()>) -> Result {
        self.check(4)?; self.backing = backing.take(); Ok(())
    }
}
impl Drop for Storage { fn drop(&mut self) { self.trace.borrow_mut().dropped_storage += 1; } }
struct G17PRetiredRenderGraph { bind: G17PRenderRootOwner, context_aliases: Option<()>, storage: Storage }
struct Resources;
impl Resources {
    fn rebind_cached_render_vm(&mut self, storage: &mut Storage, _: &mmu::Vm, _: &mut ()) -> Result {
        storage.check(2)
    }
}
struct Cache(Vec<G17PRetiredRenderGraph>);
struct PushError(G17PRetiredRenderGraph);
impl Cache {
    fn push_within_capacity(&mut self, graph: G17PRetiredRenderGraph) -> std::result::Result<(), PushError> {
        self.0.push(graph); Ok(())
    }
}
struct Sksm { backing: Option<()> }
struct Manager {
    resources: Resources, render_sksm: Option<Sksm>,
    retired_render_handoff_graphs: Cache, render_submission_ordinal: u32,
    active_render_slot: u8, render_bind: Option<G17PRenderRootOwner>,
    render_context_aliases: Option<()>, render_storage: Option<Storage>,
}
impl Manager {
''' + text[start:end] + r'''
}
fn exercise(fail: u8) {
    let trace = Rc::new(RefCell::new(Trace { fail, ..Trace::default() }));
    let root = |id| Arc::new(mmu::T8140ComputeExecutionContext { vm: mmu::Vm(id), trace: trace.clone() });
    let old = root(1);
    let graph = G17PRetiredRenderGraph {
        bind: G17PRenderRootOwner::Job { context: old.clone() }, context_aliases: Some(()),
        storage: Storage { trace: trace.clone(), context: Some(old), backing: None },
    };
    let mut manager = Manager {
        resources: Resources, render_sksm: Some(Sksm { backing: Some(()) }),
        retired_render_handoff_graphs: Cache(vec![]), render_submission_ordinal: 42,
        active_render_slot: 0, render_bind: None, render_context_aliases: None, render_storage: None,
    };
    let result = manager.reactivate_recycled_render_graph(graph, &mmu::Vm(2), root(2));
    assert_eq!(trace.borrow().dropped_storage, 0);
    if fail == 0 {
        assert_eq!(result, Ok(()));
        assert!(manager.render_storage.is_some());
        assert!(manager.retired_render_handoff_graphs.0.is_empty());
        assert_eq!(trace.borrow().dropped_roots, [1]);
        assert!(manager.render_sksm.as_ref().unwrap().backing.is_none());
    } else {
        assert_eq!(result, Err(EIO));
        assert!(manager.render_storage.is_none());
        assert_eq!(manager.retired_render_handoff_graphs.0.len(), 1);
        assert!(!trace.borrow().dropped_roots.contains(&1));
        if fail > 1 { assert!(!trace.borrow().dropped_roots.contains(&2)); }
        assert!(manager.render_sksm.as_ref().unwrap().backing.is_some());
    }
    drop(manager);
    assert_eq!(trace.borrow().dropped_storage, 1);
}
#[test] fn success_transfers_the_complete_graph() { exercise(0); }
#[test] fn retarget_failure_retains_old_root_and_graph() { exercise(1); }
#[test] fn alias_failure_retains_both_roots_and_graph() { exercise(2); }
#[test] fn cursor_failure_retains_both_roots_and_graph() { exercise(3); }
#[test] fn claim_failure_retains_both_roots_and_graph() { exercise(4); }
'''
with tempfile.TemporaryDirectory(prefix="g17-rebind-failure-") as temp:
    src = Path(temp) / "test.rs"
    binary = Path(temp) / "test"
    src.write_text(source)
    subprocess.run(["rustc", "--edition=2021", "--test", str(src), "-o", str(binary)], check=True)
    subprocess.run([str(binary)], check=True)

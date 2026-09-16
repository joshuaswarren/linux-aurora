#!/usr/bin/env python3
"""Compile the driver's cache selector and test owner/retirement precedence."""

from pathlib import Path
import subprocess
import tempfile

manager = (Path(__file__).resolve().parents[1] / "g17_manager.rs").read_text()


def function(marker):
    start = manager.index(marker)
    end = manager.index("{", start) + 1
    depth = 1
    while depth:
        depth += (manager[end] == "{") - (manager[end] == "}")
        end += 1
    return manager[start:end]


source = r'''
#![allow(dead_code, unused_variables)]
macro_rules! dev_info { ($($arg:tt)*) => {}; }
mod util { pub fn g17p_verbose() -> bool { false } }
mod mmu {
    pub struct Vm { pub id: u64, pub pools: [u64; 2] }
    impl Vm {
        pub fn render_pool_mappings_match(&self, pair: u8, pool: u64) -> bool {
            self.pools[pair as usize] == pool
        }
    }
}
struct Bind(u64);
impl Bind { fn matches(&self, vm: &mmu::Vm) -> bool { self.0 == vm.id } }
struct Pair(u8);
impl Pair { fn slot(&self) -> u8 { self.0 } }
struct Storage { pair: u8, context: u16, pool: u64, addresses_available: bool }
impl Storage {
    fn queue_pair(&self) -> Pair { Pair(self.pair) }
    fn descriptor_context_id(&self) -> u16 { self.context }
    fn render_pool_id(&self) -> u64 { self.pool }
    fn client_rebind_addresses_available(&self, _vm: &mmu::Vm) -> bool {
        self.addresses_available
    }
}
struct G17PRetiredRenderGraph {
    bind: Bind,
    storage: Storage,
    retire_after_context_epoch: u64,
}
struct Manager {
    active_render_slot: u8,
    render_context_epochs: [u64; 2],
    released_render_contexts: u64,
    retired_render_handoff_graph: Option<G17PRetiredRenderGraph>,
    retired_render_handoff_graphs: Vec<G17PRetiredRenderGraph>,
}
impl Manager {
''' + "\n".join(function(marker) for marker in (
    "fn retained_graph_matches_owner(",
    "fn select_cached_render_graph(",
    "fn retired_render_graph_reclaimable(",
)) + r'''
}
fn graph(vm: u64, context: u16, pair: u8, epoch: u64) -> G17PRetiredRenderGraph {
    G17PRetiredRenderGraph {
        bind: Bind(vm), storage: Storage { pair, context, pool: vm * 10, addresses_available: true },
        retire_after_context_epoch: epoch,
    }
}
fn manager() -> Manager {
    Manager {
        active_render_slot: 0, render_context_epochs: [10, 10],
        released_render_contexts: 1 << 4,
        retired_render_handoff_graph: None, retired_render_handoff_graphs: vec![],
    }
}
fn vm() -> mmu::Vm { mmu::Vm { id: 2, pools: [20, 20] } }

#[test]
fn own_graph_in_vector_precedes_reusable_immediate_foreign_graph() {
    let mut m = manager();
    m.retired_render_handoff_graph = Some(graph(1, 4, 0, 9));
    m.retired_render_handoff_graphs.push(graph(2, 5, 0, 11));
    assert_eq!(m.select_cached_render_graph(&vm(), 5), (false, Some(0)));
}
#[test]
fn own_graph_precedes_reusable_foreign_graph_at_every_vector_position() {
    for own in 0..8 {
        let mut m = manager();
        m.retired_render_handoff_graphs = (0..8).map(|i| {
            if i == own { graph(2, 5, 0, 11) } else { graph(1, 4, 0, 9) }
        }).collect();
        assert_eq!(m.select_cached_render_graph(&vm(), 5), (false, Some(own)));
    }
}
#[test]
fn immediate_own_graph_is_retained() {
    let mut m = manager();
    m.retired_render_handoff_graph = Some(graph(2, 5, 0, 11));
    m.retired_render_handoff_graphs.push(graph(1, 4, 0, 9));
    assert_eq!(m.select_cached_render_graph(&vm(), 5), (true, None));
}
#[test]
fn foreign_reuse_requires_both_release_and_completed_epoch() {
    for released in [false, true] {
        for complete in [false, true] {
            let mut m = manager();
            m.released_render_contexts = if released { 1 << 4 } else { 0 };
            m.retired_render_handoff_graphs.push(graph(1, 4, 0, if complete { 10 } else { 11 }));
            let expected = if released && complete { Some(0) } else { None };
            assert_eq!(m.select_cached_render_graph(&vm(), 5), (false, expected));
        }
    }
}
#[test]
fn another_physical_pair_cannot_supply_a_graph() {
    let mut m = manager();
    m.retired_render_handoff_graph = Some(graph(2, 5, 1, 9));
    m.retired_render_handoff_graphs.push(graph(1, 4, 1, 9));
    assert_eq!(m.select_cached_render_graph(&vm(), 5), (false, None));
}
#[test]
fn occupied_embedded_addresses_require_another_graph() {
    let mut m = manager();
    let mut conflict = graph(1, 4, 0, 9);
    conflict.storage.addresses_available = false;
    m.retired_render_handoff_graph = Some(conflict);
    m.retired_render_handoff_graphs.push(graph(3, 4, 0, 9));
    assert_eq!(m.select_cached_render_graph(&vm(), 5), (false, Some(0)));
    m.retired_render_handoff_graphs.clear();
    assert_eq!(m.select_cached_render_graph(&vm(), 5), (false, None));
}
#[test]
fn same_vm_with_wrong_pool_or_context_is_not_cross_vm_rebound() {
    let mut m = manager();
    m.retired_render_handoff_graphs.push(graph(2, 4, 0, 9));
    assert_eq!(m.select_cached_render_graph(&vm(), 5), (false, None));
    let stale = mmu::Vm { id: 2, pools: [99, 99] };
    assert_eq!(m.select_cached_render_graph(&stale, 4), (false, None));
}
'''

with tempfile.TemporaryDirectory(prefix="g17-render-cache-") as temp:
    src = Path(temp) / "test.rs"
    binary = Path(temp) / "test"
    src.write_text(source)
    subprocess.run(["rustc", "--edition=2021", "--test", str(src), "-o", str(binary)], check=True)
    subprocess.run([str(binary)], check=True)

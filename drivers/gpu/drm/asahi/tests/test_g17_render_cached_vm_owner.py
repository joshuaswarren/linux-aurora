#!/usr/bin/env python3
"""Execute the real cached-owner predicate across logical queue changes."""
from pathlib import Path
import subprocess
import tempfile

source = (Path(__file__).resolve().parents[1] / "g17_manager.rs").read_text()
start = source.index("    fn retained_graph_matches_vm(")
end = source.index("    /// The retained graph and global aliases", start)
predicate = source[start:end]

code = r'''
#![allow(dead_code)]
mod mmu {
    pub struct Vm { pub id: u64, pub pools: [u64; 2] }
    impl Vm {
        pub fn render_pool_mappings_match(&self, slot: usize, pool: u64) -> bool {
            self.pools[slot] == pool
        }
    }
}
struct Bind(u64);
impl Bind { fn matches(&self, vm: &mmu::Vm) -> bool { self.0 == vm.id } }
struct Pair(usize);
impl Pair { fn slot(&self) -> usize { self.0 } }
struct Storage { context: u16, pair: Pair, pool: u64 }
impl Storage {
    fn descriptor_context_id(&self) -> u16 { self.context }
    fn queue_pair(&self) -> &Pair { &self.pair }
    fn render_pool_id(&self) -> u64 { self.pool }
}
struct G17PRetiredRenderGraph { bind: Bind, storage: Storage }
struct Manager;
impl Manager {
''' + predicate + r'''
}
#[test]
fn alternating_clients_restore_the_vm_graph_after_a_queue_change() {
    let vm = mmu::Vm { id: 15, pools: [101, 102] };
    let compositor = G17PRetiredRenderGraph {
        bind: Bind(2), storage: Storage { context: 5, pair: Pair(0), pool: 201 }
    };
    let application = G17PRetiredRenderGraph {
        bind: Bind(15), storage: Storage { context: 8, pair: Pair(0), pool: 101 }
    };
    // Queue 8's graph is cached while the compositor owns the pair. Queue 9
    // of that same application must find it before allocating fixed RT/TPC VAs.
    let incoming_context = 9;
    let cache = [compositor, application];
    let graph = cache.iter().find(|g| Manager::retained_graph_matches_vm(g, &vm)).unwrap();
    assert_eq!(graph.storage.pool, 101);
    assert_ne!(graph.storage.context, incoming_context);
    assert!(!Manager::retained_graph_matches_vm(&cache[0], &vm));
}
#[test]
fn stale_pool_identity_is_not_reused() {
    let vm = mmu::Vm { id: 15, pools: [101, 102] };
    let stale = G17PRetiredRenderGraph {
        bind: Bind(15), storage: Storage { context: 8, pair: Pair(1), pool: 101 }
    };
    assert!(!Manager::retained_graph_matches_vm(&stale, &vm));
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = Path(directory)
    (path / "cached.rs").write_text(code)
    subprocess.run(["rustc", "--edition=2021", "--test", str(path / "cached.rs"),
                    "-o", str(path / "cached")], check=True)
    subprocess.run([str(path / "cached")], check=True)

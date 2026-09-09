#!/usr/bin/env python3
"""Installed compute graphs recycle by exact retirement, not client count."""

from dataclasses import dataclass
from pathlib import Path


root = Path(__file__).resolve().parents[1]
runtime = (root / "g17_async_compute.rs").read_text()
limits = (root / "g17_queue_limits.rs").read_text()


def block(source, marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


prepare = block(runtime, "    pub(crate) fn prepare_async_compute_queue(")
matches = block(runtime, "    pub(crate) fn async_compute_queue_matches(")
release = block(runtime, "    pub(crate) fn release_async_compute_queue(")
service = block(runtime, "    fn service_async_compute_completions(")
needs_install = block(runtime, "    pub(crate) fn async_compute_needs_install(")
predicate = block(limits, "pub(crate) const fn reusable_idle_compute_graph(")

for field in ["q.owner.is_none()", "q.active.is_none()", "q.binding.is_none()", "q.context.is_none()"]:
    assert field in prepare
assert "q.quarantined" in prepare
selection = prepare[: prepare.index("let index =")]
assert "requires_first_install" not in selection
assert "without reinstall" in prepare
assert "return Ok(false)" in matches
for identity in [
    "q.queue.queue_id() != hardware_queue",
    "!Arc::ptr_eq(installed_context, context)",
    "!context.is_current()",
    "q.binding.is_none()",
]:
    assert identity in matches
assert "q.released" in matches and "q.quarantined" in matches
assert "owner_free && active_free && binding_free && context_free && !quarantined" in predicate
assert "q.active.is_none() && !q.quarantined" in release
assert release.index("q.released = true") < release.index("q.binding = None")
assert service.index("q.active.take()") < service.index("if q.released")
assert "q.binding = None; q.context = None; q.owner = None" in service
assert "q.queue.requires_first_install()" in needs_install


@dataclass
class Graph:
    qid: int
    installed: bool = False
    owner: int | None = None
    active: bool = False
    binding: bool = False
    context: int | None = None
    released: bool = False
    quarantined: bool = False
    installs: int = 0

    def reusable(self):
        return (
            self.owner is None
            and not self.active
            and not self.binding
            and self.context is None
            and not self.quarantined
        )


class Pool:
    def __init__(self):
        self.graphs = []
        self.next_qid = 4

    def bind(self, owner, context):
        graph = next((graph for graph in self.graphs if graph.reusable()), None)
        if graph is None:
            assert self.next_qid <= 127, "finite QID namespace leaked"
            graph = Graph(self.next_qid)
            self.next_qid += 1
            self.graphs.append(graph)
        graph.owner = owner
        graph.binding = True
        graph.context = context
        graph.released = False
        needs_first_install = not graph.installed
        if needs_first_install:
            graph.installed = True
            graph.installs += 1
        return graph, needs_first_install

    @staticmethod
    def submit(graph):
        assert not graph.active and not graph.quarantined
        graph.active = True

    @staticmethod
    def release(graph):
        graph.released = True
        if not graph.active and not graph.quarantined:
            graph.binding = False
            graph.context = None
            graph.owner = None

    @staticmethod
    def complete(graph):
        graph.active = False
        if graph.released:
            graph.binding = False
            graph.context = None
            graph.owner = None


# Ten thousand sequential clients exercise far beyond the 124 usable compute
# QIDs while retaining exactly one installed graph and installing QID4 once.
pool = Pool()
for owner in range(10_000):
    graph, install = pool.bind(owner, 5)
    assert graph.qid == 4
    assert install == (owner == 0)
    pool.submit(graph)
    pool.complete(graph)
    pool.release(graph)
assert len(pool.graphs) == 1 and pool.graphs[0].installs == 1

# The maximum context population (55) allocates 55 distinct graphs. Release
# them in a scrambled order, then run thousands of waves with changed
# graph/context coupling. No installed QID is duplicated or reinstalled.
pool = Pool()
peak = 55
wave = []
for index in range(peak):
    graph, install = pool.bind(index, 5 + index)
    assert install and graph.qid == 4 + index
    pool.submit(graph)
    wave.append(graph)
for index in list(range(0, peak, 2)) + list(range(1, peak, 2)):
    graph = wave[index]
    pool.release(graph)  # close while active retains owner/root/context
    assert not graph.reusable()
    pool.complete(graph)
    assert graph.reusable()

for wave_id in range(2_000):
    live = []
    for index in range(peak):
        # Reverse contexts every other wave so reuse is not accidentally tied
        # to the QID's original GART-context identity.
        context = 5 + (peak - 1 - index if wave_id & 1 else index)
        graph, install = pool.bind(100_000 + wave_id * peak + index, context)
        assert not install
        pool.submit(graph)
        live.append(graph)
    for graph in reversed(live):
        pool.complete(graph)
        pool.release(graph)

assert len(pool.graphs) == peak
assert [graph.qid for graph in pool.graphs] == list(range(4, 4 + peak))
assert all(graph.installs == 1 for graph in pool.graphs)

# A faulted graph retains all firmware-nameable state and never aliases a new
# owner. The allocator grows around it without reusing its QID.
faulted, _ = pool.bind(999_999, 5)
pool.submit(faulted)
faulted.quarantined = True
pool.release(faulted)
assert not faulted.reusable() and faulted.owner == 999_999
replacement, install = pool.bind(1_000_000, 6)
assert replacement is not faulted and not install
assert replacement.qid != faulted.qid

print("PASS compute graph reuse: 10k serial + 2k x 55 overlap waves, one install/QID, close-active and quarantine safe")

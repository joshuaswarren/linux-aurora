# G17P render graph reuse qualification

Measured on Neo, 2026-09-16, kernel `7.1.6-neo-g17p2`.

The complete render graph remains cached after queue destruction and context
supersession. Reuse retains its queue records, descriptors, support, PM/TVB
backing and reserved HardwareBufferID. The incoming VM receives client aliases
and a new command context. Foreign reuse requires the recorded context epoch,
queue release, and an unoccupied destination alias set. Returning owners take
precedence over reusable foreign graphs.

The destination check is necessary even when the graph's old owner has retired.
The r8/r9 captures show an incoming desktop VM already mapping
`0x7040040000..0x7040058000`, which a cached graph needed for render state. Another
capture shows an occupied diagnostic descriptor alias at `0x704003c000`.
Rejecting those candidates preserves the existing mappings and allows another
cached graph or normal allocation to be selected.

## Final tested module

- Module SHA-256: `b457bb99ca641a8b12574934129d59e50aab9b241d4f31b2f93f80d0e0d1cca1`
- Mesa candidate: `k4-blend-mrt3-r2`
- Mesa SHA-256: `daffdeba4c6e63e33e24a67ee385a9026c11f6506c82b8dab297cdcc0bfca028`
- Test: `dEQP-VK.query_pool.occlusion_query.basic_conservative`, each iteration in
  a fresh process, shader cache disabled, Vulkan version override 1.0.
- Desktop present: 300/300 iterations passed.
- Headless, same firmware session after stopping the display manager: 300/300
  iterations passed.
- Whole boot: 593 graph rebinds, 11 PM generations, no recorded submission
  failures, firmware recovery requests, or session-crash markers.

Neo evidence directories:

- `/opt/neo-gpu/results/drm-graph-rebind-r10-desktop300`
- `/opt/neo-gpu/results/drm-graph-rebind-r10-headless300`

Both contain per-process QPA results, module/driver hashes, boot ID and kernel
log window. This is process-lifetime stress evidence, not Vulkan 1.4 mustpass
qualification or a throughput benchmark.

## Host regressions

Eleven focused scripts passed. Three compile the actual driver functions or
closure into Rust tests: cache selection has seven cases, alias availability
has five, and rebind fault injection has five. They cover returning-owner
precedence, physical-pair isolation, both retirement prerequisites, destination
overlaps, page alignment and overflow, and graph/root retention at every
fallible rebind stage. Existing constructor, root, SKSM, PM and HardwareBufferID
checks also passed.

Partial PM/support reinitialization and the experimental blocking graph wait
are absent. Quarantined PM/support objects retain their IDs until processor
shutdown and cannot be used as constructor input.

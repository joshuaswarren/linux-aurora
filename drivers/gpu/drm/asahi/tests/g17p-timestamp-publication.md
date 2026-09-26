# G17P timestamp publication ordering

Firmware completion tokens order GPU execution. They do not order the later
CPU copy from the driver's private timestamp storage into a user query buffer.
A post-render timestamp-copy shader could run before that copy, propagate the
query's `UINT64_MAX` sentinel, and leave a subsequent query-result shader waiting
forever. SuperTuxKart through Zink reproduced this as a compute timeout followed
by device loss and rejected buffer mappings.

Commands with user timestamp destinations now expose their completion fence as
the readiness dependency for consumers. That fence signals after the CPU writes
the timestamps. Other commands retain publication dependencies and native GPU
ordering. Both indexed render-prefix dependencies and cross-engine dependencies
use the readiness fence; independent queues remain independent.

Timeout snapshots also record the retained compute stream's base and terminator,
without following firmware pointers or reading GPU registers. This identified
the failing 26-dispatch stream in the SuperTuxKart capture.

## Hardware regression

`g17p_elapsed_query_probe.c` issues 14 elapsed-time queries per frame, including
empty queries and multiple queries in one render pass, then consumes their
results. It checks 32 frames (448 queries) and rejects GL errors and invalid
elapsed times. Run on the test Neo with the accelerated Zink/Honeykrisp stack:

```sh
cc -O2 g17p_elapsed_query_probe.c -o g17p-elapsed-query-probe -lEGL -lGL
timeout -s KILL 45 ./g17p-elapsed-query-probe
```

The timeout limits the process, not a hardware hang. Verify the printed renderer
and inspect the kernel log after the run. A failed GPU session requires a reboot
before another comparison.

On Neo, 2026-09-27, with agx3 Mesa `10081cd` and kernel base `e8077ec05`:

- Before the ordering fix, the probe loses the device on frame 0 and reports an
  invalid elapsed time. The full SuperTuxKart/Zink replay times out at the same
  frame on clean boots, including with serialized submits, batching disabled,
  and stronger dispatch cache flushes.
- The initial render-ordering fix passes all 448 queries and the full replay,
  producing all three expected screenshots without a GPU fault.
- The final build covers render and compute timestamp producers. On boot
  `8b0db2ec-a652-4bc2-a40e-3d180427f2ae`, the 448-query probe, two
  SuperTuxKart/Zink replays, native Vulkan SuperTuxKart, and both Vulkan and Zink
  0 A.D. replays pass without GPU faults. Each replay produces its three
  screenshots. These runs use the normal installed Mesa with no synchronization,
  batching, or cache-flush overrides.
- The SuperTuxKart/Zink screenshots were inspected and compared with the frozen
  M4 capture: mean RGB differences are below 0.006 levels on a 0–255 scale.
  Sparse pixel differences remain; this is not a bit-exact image pass.
- The 14 cross-engine/frontier host tests and the three render UMA handoff tests
  pass. The module builds successfully. Thermal throttling remains enabled;
  hardware runs retain the owner's 120°C stop threshold and reach at most
  104.261°C in the five final replays.

Final uncompressed module SHA-256:
`836bf06ae5fef8c0e8bc7d5b63d8d711935aca4613c85cc2316821d1d2fc8eed`.

Evidence is retained under
`/home/council/omarchy-builds/neo-image-diagnosis-2026.09.27/`, including baseline
and patched logs, source/build identities, replay captures, telemetry, and image
comparisons. These are focused Neo checks, not full Vulkan/OpenGL conformance or
qualification of other machines.

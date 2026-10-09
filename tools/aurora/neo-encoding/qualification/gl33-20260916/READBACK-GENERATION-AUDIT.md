# Render/completion generation identity: 2026-09-15

Status: focused hardware validation passed; broader fixed-build CTS qualification
remains outstanding. Correcting this native/Linux generation mismatch removed
the observed stale first reads in two complete probes and passed the original
55 failure/incomplete CTS executions. This does not establish full conformance.

## Provenance and observed data flow

Read-only static analysis of the local 25G83 kernelcache and B1 firmware, using
`probes/native_b1.py`. This evidence comes from binary analysis, not a claim of
an independently isolated clean-room implementation.

- Kernelcache SHA-256: `b1ea68d23c452099da9fbfe06b19634e0175d4b2b1194f7249abac430a9a7a0d`.
- IPSW `Firmware/agx/armfw_g17p.im4p` SHA-256:
  `9b2afdfbf6c3d4b2892a1e4b98766d57a322c95a2a3a535403c50b2dbfa2955b`.
- B1 image SHA-256: `c1d35018d1226f8b7db374b0cc4332c6e43479ad7a0d001c5a7d0ecda1354412`.

`AGXCommandBuffer::nextCommandBufferState` (0xfffffe0008862d94..0xfffffe0008862fc8)
advances a 36-entry physical ring independently from a shared generation
allocator. It reads the shared next generation at owner +0x180, skips unavailable
slots, advances the allocator, and stores the allocated `w22` into EventControl
+8 at 0xfffffe0008862f18 (`stp w22, wzr, [x21, #8]`).

`AGXCommandDescriptor::loadCommandBufferData` stores GPU and CPU pointers for
that same record into descriptor +0x188/+0x190 at 0xfffffe000886288c. The native
fragment submission copies +0x188 into raw firmware descriptor +0x20 at
0xfffffe00088379cc..0xfffffe00088379d0.

The 3D register builder loads descriptor +0x190, then record +8, masks the
generation to six bits, and ORs it with context ID shifted eight bits:
0xfffffe00088c04cc..0xfffffe00088c04dc. The TA builder performs the same operation
at 0xfffffe00088bdd28..0xfffffe00088bdd38. This establishes common source identity
for the render work key and the completion record's generation.

B1 (addresses relative to 0xfffffc0000000000) loads the raw descriptor's +0x20
record pointer at 0xc9f0 and copies descriptor context ID to record +0x24 at
0xd254. Completion reads record +8 and +0x24 at 0x16b6c/0x16b70. It uses the
generation's low six bits in the SGX +0x18030 request at 0x16bb8..0x16bcc, polls
the completion status, and subsequently publishes the fragment stamp at 0x16ea0.
The selected instruction witnesses are mechanically checked by
`probes/verify_readback_generation.py`; those checks are not a whole-function
control-flow proof.

## Linux defect and candidate

Candidate: `neo-encoder-linux-j700-readback-20260915`, branch
`j700-readback-publication-20260915`, base `8b31b4760ee2`.
The final two-file correction is committed as `4863a3c90a3b`; the checkout is
isolated from the original dirty `j700` trees and has not been pushed.

The shared `G17PCommandBufferStateAllocator` already allocates the render work
key correctly. Both TA and fragment receive the same generation. But the cold
completion record retains a captured value, while the retained record writes
`submission_ordinal + submission_ordinal / 2`. These are different namespaces;
neither identifies the allocator's actual generation across reuse and clients.

The candidate writes the actual allocated generation to record +8 in both paths.
The existing internal two-tag API is retained: normal render submissions pass
the same allocated generation twice; the separate driver-owned empty bootstrap
passes tags 0/1 and retains its original record generation 0. An initial build
caught this additional caller, so the proposed single-tag API refactor was
removed instead of changing the bootstrap behavior in this experiment.
The candidate adds no queue submissions, CPU delays, retries, extra fences,
cache barriers, or draw splitting. Other ordinal-derived descriptor fields are
unchanged because the evidence here does not establish their meaning.

## Validation and disproof criteria

The new descriptor regression passes across different contexts and six-bit tag
wrap. Full host descriptor suite: candidate 18 pass / 3 fail; exact unmodified
base 17 pass / the same 3 fail. Existing failures are old descriptor/register
hash expectations and the cold-opening override expectation. No fixture hashes
were refreshed to hide them. Log: `readback-publication-descriptor-tests.log`.

Build the candidate separately from the known baseline source/build. Verify
source hashes and the loaded module's build ID. Keep baseline Mesa
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2` fixed.
Use the unchanged 12,000-iteration readback reproducer. Any stale/unstable read,
GL error, or kernel fault rejects the proposed explanation as a sufficient fix.
If it passes, repeat and replay the original 55 failure/incomplete executions.
These results do not substitute for a complete fixed-build official CTS run.

Build completed successfully after preserving the bootstrap API. Module:
`candidates/asahi-kernel-readback-generation.ko`, SHA-256
`03abec7b984762e6b32a27951652d4c0445e8300e7447a795e5c45dc5291d762`, ELF build ID
`81b0fb722ffe014f1a72eb53436c290c7ad52f39`. All 77 Rust file hashes matched the
isolated builder after the final edit. Build log:
`readback-publication-kernel-build-2.log`. Final patch SHA-256:
`dc677945afc4f3c150a8ea03df1f4cadd5cff2b644d1b60dfa82ac810850e3d8`.

Fresh unchanged baseline (`readback-generation-before-20260915`): failed at
iteration 130, 118 checked, one mismatch/unstable first read, second read
correct, no kernel fault. Boot `fcc7cfaf-1f57-4d12-84a4-debecd791a6f`, Xorg 749,
baseline Mesa/kernel hashes above.

First candidate hardware result (`readback-generation-after-20260915`): PASS,
12,000/12,000 iterations, 11,988 pixel comparisons, zero mismatches or unstable
reads, no kernel fault. Probe time 20.354 seconds. Boot
`9f1434bf-725b-4ff1-ae51-babd4df25cb4`, Xorg 717. Preparation verified the loaded
module's GNU build note matches the candidate file; Mesa remained the pinned
baseline. This is positive evidence for the hypothesis, not full qualification.
Repeat probe (`readback-generation-repeat-20260915`): PASS, another 12,000
iterations / 11,988 comparisons, zero mismatches or unstable reads, no fault,
20.621 seconds. Together: 24,000 iterations and 23,976 comparisons. Both
receipts bind the results to the candidate loaded module and unchanged Mesa.

Original failure/incomplete replay (`readback-generation-replay-20260915`):
55/55 Pass, six original configuration groups (10, 10, 6, 10, 8, 11 executions),
no missing/duplicate/NotSupported substitutions, no kernel fault or kernel
stream overrun. The pinned official CTS commit remains
`067e8832315e79817ede1c4863804e440f5d1c80`; total CTS time 130.237 seconds.

Current hardware is healthy and idle on boot `9f1434bf-725b-4ff1-ae51-babd4df25cb4`,
dedicated Xorg 717, candidate kernel and normal Mesa. Qualified RAM boot used;
resident unchanged. Original source trees' dirty-diff hashes remain unchanged.
No full official campaign has yet run on this fixed kernel. Next: additional
reuse/concurrency and broader CTS regression, followed by a complete campaign
with a fixed build and explicit accounting of supported/unsupported executions.

## Debugging workflow adopted from the article

Source: https://codyho.dev/blog/gpu-driver/

Their useful lessons are to let concrete driver failures direct research, reduce
captures before analysis, and compare known-good GPU state with failing state.
For this investigation, maintain a short evidence packet containing exact build
hashes, the reproducer, observations, competing explanations, and one predicted
effect per candidate. Model-generated explanations remain hypotheses until the
hardware discriminates between them.

The earlier weak/strong CDM experiment is an example: both controls passed while
the unchanged baseline failed before and after. It showed that added work/wait
can mask the race, not that stronger cache maintenance is necessary. Neither
diagnostic variant is the production fix. See `HOST-READ-CDM-EXPERIMENT.md`.

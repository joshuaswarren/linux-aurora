# MSAA fragment descriptor sample count

The installed source is `neo-encoder-linux` at
`c85eec3bf95b22b15f87bedf5cb29717b01586f8`. The actual G17P path is
`g17_uapi.rs` -> `g17_render.rs`, not the legacy `queue/render.rs`.
Translation correctly encodes log2(samples) in UTILE_CONFIG and forwards the
sample positions, but the firmware fragment descriptor omitted the count at
offset 0x50. Only the cold-opening builder wrote a literal one.

## Independent native evidence

The local restored 25G83 kernel collection is
`neo-update/kernel/25G83/kernelcache.release.mac17p.decompressed`, SHA256
`b1ea68d23c452099da9fbfe06b19634e0175d4b2b1194f7249abac430a9a7a0d`.
Its `com.apple.AGXG17P` fileset entry is at file offset 0x131120. The
__TEXT_EXEC segment maps file offset 0x1832e70 to VA 0xfffffe0008836e70.
Native symbol names and Capstone disassembly establish the complete copy path:

- `AGXRenderTargetMemoryG16::halComputeGeometry`, 0xfffffe00088c4ca8:
  loads the sample count from target +0x48; branches on 1/2/4/8 to assemble
  the corresponding sample positions and tile configuration.
- 0xfffffe00088c4e10: stores that count into target +0xb8. Exact bytes:
  `6c2a17297f5a00f99f0d0071cc0000549f050071200600549f09007180050054`.
- `AGX3DCommandDescriptor::loadRenderTargetMemoryData`,
  0xfffffe0008839d80..0x8839d90: copies target +0xb8 into host +0x8c8,
  alongside the sample-position word and macro geometry.
- `AGX3DChannelSKSM::submitBuffer`, 0xfffffe0008837af8..0x8837b08:
  copies host +0x8c8 into firmware descriptor +0x50. This is the ordinary
  submission path, not first-work lifecycle metadata.

The correction derives the count from the already validated UTILE_CONFIG
sample mode, writes it in the common fragment builder, and removes the
incorrect first-work literal. This adds no GPU instruction, synchronization,
submission, or capability restriction. The existing four-draw source batch
remains intact.

## Validation and candidate

- `kernel-msaa-samples-red.log`: new real-builder regression fails before the
  fix (retained 1x descriptor has zero, expected one).
- `kernel-msaa-samples-green.log`: passes all 1x/2x/4x first and retained checks.
- `compare-kernel-msaa-tests.py` builds HEAD and candidate with the same
  parameter stubs. Baseline: 16 pass/3 fail. Candidate: 17 pass/3 fail. The
  same three existing native-fixture tests fail in this host harness;
  `kernel-msaa-regressions.json` records their exact identities. No golden
  expectations were changed or failures suppressed.
- All 77 tracked Asahi Rust source files in the build VM match the local
  candidate. `kernel-msaa-build.log`: module build completed successfully.
- `prepare-kernel-msaa.py` verifies the qualified boot bundle, replaces only
  its Asahi module, and records source patch/module hashes in the new bundle.
  Kernel Image, resident, device tree, other modules and SSD layout are retained.

Hardware validation is pending. Last read-only SSH check confirmed the
faulted boot c787cb00-913b-4cf3-aad2-78f68d1f7ab3 and no Xorg/glcts.
Use a fresh guarded RAM boot before any GPU submissions. Preparation now
accepts an explicit third argument for the expected Asahi module SHA256;
its default remains the old qualified module. Supply the candidate hash.
Then run the exact maximum-height colour and depth CTS cases and, if healthy,
the unchanged complete official GL3.0 suite. The prior full-suite counts have
not changed; completion remains unproven.

## Installed-root qualification

The first qualified RAM boot passed resident, Stage2, kernel and initramfs
verification and reached Linux af131ac0-be8f-493c-b3fc-e1337f15713e. The
installed-root init script does not load Asahi before switch_root: systemd
loads it from the SSD. SSH confirmed the old module and old loaded build ID,
so this boot did not test the candidate and no GPU tests were run.

`install-asahi-msaa-samples.py` verifies target, boot, process state and both
binary hashes; retains the old module; atomically stages the candidate; and
does not unload/reload the GPU. It completed successfully, followed by a
guarded normal reboot. Candidate module SHA256 is
`aa5b21014671517849cfa12c7415cd5b39f565283c7140f3b3ebaa367e96fb4b`;
its GNU build ID is `a947f573e40321482d8e76da54c1663561075deb`.
The old file remains at
`/home/mcruz/gl30-cts-20260913/asahi-before-msaa-samples.ko` on the Neo.
CTS preparation now checks both the expected file SHA and equality between
the ELF build note and `/sys/module/asahi/notes/.note.gnu.build-id`.

`verify-native-msaa-count.py` independently rechecks the native kernel SHA,
Mach-O mapping and four exact instruction blocks, saving
`native-msaa-count-receipt.json`. It passes. The kernel milestone was posted
to Discord using `discord-kernel-msaa-samplecount.json`.

## Next independent CTS diagnostic

The prior full run's `runner.log` contains 15 messages saying that the
geometry program was refused by the backend. Mesa implements transform
feedback through a passthrough GS (`agx_state.c` around 6267), and rejects
unusable compiled GS programs before launching them (around 7008).
This gives a concrete next diagnostic for the previously explained zero
clip-distance feedback readback: run official
`KHR-GL30.clip_distance.coverage` with the existing shader trace option to
identify the refused instruction. Do not change GL_MAX_CLIP_DISTANCES or
add synchronization on this evidence. The broad log's interleaved stderr
does not establish which specific rejection belongs to which case; an
isolated capture is required.

## Hardware results with the corrected module

The second normal reboot and qualified RAM boot reached
`9507522a-6422-4095-b1d9-069c35b4e181`. SSH and the preparation helper both
verified module SHA aa5b2101... and its loaded build note. The complete CTS
package verification passed, followed by DCP_DISPLAY_FRAMEBUFFER_VERIFIED.
`kernel-msaa-runtime.json` is the loaded-driver receipt.

All three runs used unchanged Mesa468edaf... and official unmodified CTS:

| Exact diagnostic | Result | CTS time |
| --- | --- | --- |
| kernel-msaa-maxcolor | Fail, no GPU fault | 0.542s |
| kernel-msaa-clip-coverage | Fail, no GPU fault | 0.294s |
| kernel-msaa-maxdepth | GPU firmware fault, unfinished QPA | 2.316s |

Colour still returns zero at(0,16383); the source retirement observer sees
524288 nonzero/full bytes, all at offsets divisible by four. Depth still
returns zero instead of0.25 at(0,16383). Both source batches retain four
draws in one submission. The sample-count correction is a native-confirmed
contract fix, but these hardware results show it is insufficient to repair
the outstanding storage/readback failure. No new full suite was run after
the reproduced GPU fault. Full-suite totals remain 2413Pass/200Fail/
9InternalError/1ResourceError/41278NotRun, plus15configurationPass.

The isolated coverage trace narrows the feedback failure beyond the broad
log hypothesis: its compute-form vertex shader has `mov_imm #0x8` followed
by device_store. The constant is correct. It then reports
`no G17P vertex program could be built for this draw`, and a compute submit
returns errno524. No generic unusable-GS-backend message appears in this
isolated case. Inspect the graphics runtime-program builder, its
prerasterization vertex use and the compute rejection before changing limits
or compiler opcodes. Preserve the null-program guard until a valid runtime
program is constructed; do not submit address zero.

The depth harness stopped Xorg711. Final read-only SSH confirmed the faulted
boot identity and no Xorg/glcts/cts-runner. All handles are terminal. The
corrected module remains staged on the SSD with the prior module backed up.
Fresh guarded RAM boot is mandatory before further GPU work. Product
`git diff --check` passes; no Mesa product sources changed in this resume.

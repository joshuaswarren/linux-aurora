# Sparse attachment identity and synchronization

> EOT interpretation corrected: see EOT-PARTITION-CORRECTION.md and IMPLEMENTATION-AUDIT.md. EOT physical partitions are offset/sample-stride quotients. Earlier EOT class-sequence and cross-partition conclusions below are historical and superseded; fragment synchronization evidence remains separate.

The compiler now preserves logical attachment identity independently of compacted tile offsets. Six native sparse-layout oracles, real fragment epilog lowering, background clear/reload compilation and state-loader release-mask checks pass. Runtime sparse BG/EOT resource selection remains incomplete; the official draw-buffers case still aborts at the layout guard.

## Implementation

- NIR local_pixel_agx load/store carry TARGET, documented as the logical attachment index; BASE remains the physical byte offset. agx_nir_lower_tilebuffer.c supplies both.
- Tile IR carries explicit colour_target; generated builders/printers expose it, and CSE equality compares it. All direct IR test callers supply their actual target identity.
- agx_compile.c preserves NIR target metadata in all four emission paths. Pre-RA ordering sorts logical targets and admits a nonzero single target. Physical payload and sample-mask rules remain checked.
- agx3_pack_binary.c stores logical targets alongside physical offsets, checks one-to-one mapping, chooses classes by logical identity, and releases a shared class after its lowest active target. Higher targets remain restricted to opaque RGBA8 and compact four-byte storage; wide blending is unsupported.
- agx3_colour_link_info, agx_linker, graphics descriptors and Ioto composers use colour_partition_mask instead of a count. Linking ORs consumed classes. The loader releases0x2ff minus each consumed class (12 shifted by twice the partition index); coverage also retains class zero. Gallium and shared Honeykrisp callers were updated together. No Air hardware was accessed.

## Independent native evidence

Apple MetalToolchain v27.1.5194.15, applegpu_g17p. All captures are offline compiler invocations without GPU execution. Missing attachments use Metal pixel_format Invalid and omit their fragment outputs. Inputs and native text hashes are in mesa-j700/src/asahi/compiler/test/probes/g17p-sparse-mrt/ and generated g17p-sparse-mrt.h.

| Active target mask | Active partitions | Native loader release |
| --- | --- | --- |
| 04 | 2 | 23f |
| 05 | 0,2 | 233 |
| 0a | 0,1 | 2c3 |
| fc | 0,2 | 233 |
| fd | 0,2 | 233 |
| fe | 0,1,2 | 203 |

Targets0,1,2 use classes12,48,192; targets3..7 share class12. If target0 is absent, the last higher target using class12 releases it before the remaining lower-target transactions. For maskfc, native releases class12 after target3, then class192 after target2 with the final bit. Neither physical ordinal nor target count describes this protocol.

native-mrt-probe.py --sparse-class-fragments captured04,05,0a,fc,fe. Earlier fragment-8-missing-attachment1 suppliesfd. make-sparse-oracle-patch.py verifies receipts, section hashes, metadata and store counts before emitting the fixture. Golden synchronization sequences and physical offsets come from native objects, not our emitter.

## Validation

- sparse-identity-build.log and sparse-identity-final-build.log: successful builds.
- sparse-identity-regressions.log:264Pass,2ExpectedFail,1unchanged pre-existing asahi-g17p-precompiled-fill robust-index golden failure. New sparse transaction and CSE tests pass, as do loader and shared Honeykrisp tests.
- sparse-background-regression.log: all six masks pass real NIR epilog, clear and texel-fetch reload compilation (18 sparse paths), alongside retained dense cases. Only this test source changed after the full CPU suite; the installed library is unchanged.
- libgallium-sparse-identity.so SHA256 c5826185f5ff6c80d176f0836b952addc19525445f1c5c409e737dca693cabfa.
- fast-iterations/sparse-identity-full/results/sparse-identity-full: unfiltered officialgl30, bootbac77c46-149d-42d4-aba1-dcae3a6422cb, Xorg1820, kernel_fault=false, CTS10.7018s, whole iteration29.9825s including candidate installation/collection. All43,916case-ledger entries exactly equal dense-mrt-full.807selectedPass,68Fail,3InternalError,1Incomplete,43022NotRun;15separate metadataPass. Still2/52sessions complete.
- No resident change, reboot, capability reduction, CTS edits, waivers, or diagnostic coverage inflation. No commits/pushes.

## EOT descriptor order is now established

Native eot-8-classes-mask04/05/0a/fc/fd/fe captures have code sizes196/238/362/1034/1068/1192bytes and D/staging bytes16/32/32/96/96/96. Driver offset arguments retain original logical indices but occupy compact even argument dwords0,2,4,... .

To establish PBE-array indexing, --compact-sparse-class-eots regenerated each with a compact attachment array and the SAME ordered synchronization partition IDs. For ALL SIX masks, complete text bytes and staging metadata are identical to the eight-slot sparse EOT. Therefore executable behavior depends on ordered active partition IDs and uses compact active PBE order. Logical IDs survive in metadata telling the host which offsets to supply. This is offline evidence, not EOT pixel qualification.

## Next work

1. Derive a general EOT emitter/variant scheme from the active ordered partition sequence; do not special-case CTS maskfd. Existing dense templates cover only0,1,2,0,... . Preserve the native cross-partition handoff that ordinary BLOCK_IMAGE_STORE sequences currently omit. Sparse-vs-compact equivalence removes the PBE indexing unknown.
2. Add an active-layout mask/helper and logical-to-active mapping in agx_state.c BG/EOT. Pack PBEs and physical offset arguments in active order. Keep logical indices for BG texture slotsrt*2 and clear uniforms4+8*rt. Current code assumes indices0..count-1, op[0] as the common operation, and contiguous layout.
3. Handle BG operation masks consistently, validate the complete program/resources, then open admission. Run draw_buffers_1 with real native-Asahi readback, followed by the entire unfiltered official suite. Compilation/first-batch submission are not pixel proof or compliance.

All native compiler, build, CPU test and hardware handles are terminal at this checkpoint. Reuse the healthy test server through fast-iteration.py, which revalidates live identity/hash/boot and serial ownership.

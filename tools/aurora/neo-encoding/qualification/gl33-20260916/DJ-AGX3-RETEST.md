# DJ AGX3 integration and official GL 3.0 retest

Status: complete. The entire official GL 3.0 schedule ran without a GPU fault.
All process handles are terminal; the Neo remains on its healthy boot and Xorg.

| Main execution result | Previous full run | Updated AGX3 run |
| --- | ---: | ---: |
| Pass | 2,413 | 40,599 |
| Fail | 200 | 3,152 |
| InternalError | 9 | 150 |
| ResourceError | 1 | 0 |
| NotRun | 41,278 | 0 |
| Scheduled total | 43,901 | 43,901 |

Pass coverage is **92.48%** of the full main schedule. All 15 configuration
checks also pass and are excluded from the table. Runtime was 441.91 seconds
(7 minutes 22 seconds), or 445.58 seconds including host orchestration.
All 52 sessions are complete (51 test sessions plus configuration checks), and
the inventory is fully accounted. This is a completed test run, not conformance:
the 3,302 failing/error executions remain unresolved.

Case-for-case comparison on the identical schedule:

- All 2,413 previously passing executions still pass.
- 15 previously failing executions now pass; none changed Pass to Fail/Error.
- 38,171 previously unrun executions now pass.
- The former ResourceError became an ordinary Fail; no GPU fault was reported.

Five case definitions recovered across the three previously reached
configurations, and now pass across all 50 configurations (250 passing
executions in this run):

- `KHR-GL30.ext_texture_shadow_lod.texturelod.sampler2darrayshadow_vertex`
- `KHR-GL30.ext_texture_shadow_lod.texturelodoffset.sampler2darrayshadow_fragment`
- `KHR-GL30.ext_texture_shadow_lod.texturelodoffset.sampler2darrayshadow_vertex`
- `KHR-GL30.ext_texture_shadow_lod.textureoffset.sampler2darrayshadow_bias_fragment`
- `KHR-GL30.ext_texture_shadow_lod.textureoffset.sampler2darrayshadow_vertex`

Remaining failures/errors by family: depth-texture wrapping 1,800; transform
feedback 400 Fail plus 150 InternalError; shadow LOD 400; transform_feedback3
150; framebuffer blits 150; clip distance 100; texture LOD bias 50; buffer
objects 50; transform_feedback2 50; API coverage 2. All 32,550 shaders30
executions pass.

Evidence: `fast-iterations/dj-agx3-full/results/dj-agx3-full/` contains the
official QPA logs, summary, receipt and complete ledgers.
`dj-agx3-coverage-comparison.json` preserves the matched execution transitions.

- Requested upstream `agx3`: `5f07e8677e39b7617457e8869dae662c98074f61`.
- `agx3-gallium`: `cc761b39c0266564a23b0882b4c3bf7de3df69e4`, already contained.
- Starting local `j700`: `1cf27f9be3d78735a857f06e2256b2c2e34fd1cc`.
- Merge commit: `4f61275aaa2ef1b3fde5c8a5da124cdfb67354ec`.
- All pre-merge local work, including untracked files, is preserved in stash
  `b095d5fce923e99c778185024e5acd9b1f99c8ab`; do not drop it during validation.

The upstream merge brought 20 commits and changed 74 files. Reapplying the local
CTS work produced nine conflicted files. Resolutions retain the upstream raw
four-target MRT protocol and local sparse/eight-target RGBA8 support, the local
MSAA sample-register correction, and both upstream and local regression tests.
The source overlay and per-file hashes are in `dj-agx3-source-manifest.json`.
The local CTS work remains uncommitted on top of the merge.

Validation used the unmodified official GL 3.0 full-run schedule and the same
CTS commit `067e8832315e79817ede1c4863804e440f5d1c80` and CTS runner SHA as
`audited-eot-full`. No case filter, trace mode, capability override or waiver
was enabled. Source-only and hardware results remain separately identified.

Neo remains on healthy boot `f80b3c8f-a9e7-4fe1-9024-1e03dcd756a4`.
The integration is installed with Mesa SHA256
`8a8162978caa19159d6a51cb2d8180ad431f43f3b3f99adce9ed986431e14c9f`.
Official `KHR-GL30.texture_lod_basic.lod_selection` passes on the same boot
without a GPU fault (`dj-agx3-lod-smoke`). Preserve resident/SSD/other Mac.

CPU results: 264 pass, two expected failures, three failures. Two added upstream
checks passed, while the two Hyprland mixed-draw tests newly fail compilation.
The precompiled-fill failure remains. The Hyprland refusal is a narrowing
FADD with an immediate and negated float32 source; it must not be reported as
a passing regression suite. MRT overlap issues were corrected and those
regressions, including the new raw four-target test, now pass.

No push was performed. Both fetched upstream tips are verified ancestors of
local `j700`; the preserved local CTS fixes remain uncommitted, as before.
Read `PROGRESS_WEBHOOK_GUIDE.md` at
`/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md`
in full after compaction before further actions.

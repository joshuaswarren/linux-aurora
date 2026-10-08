# Official framebuffer multisample test: next implementation boundary

The full native GL3.0 CTS run with Mesa `b4830bba7ad5e16f353c957903c5e3df53aeb0855108e64b62ae4db1c9958968` reaches `KHR-GL30.framebuffer_blit.multisampled_to_singlesampled_blit_color_config_test` and aborts at the `background tile layout` guard in `agx_state.c`. The full run and isolated rerun both have `kernel_fault=false`. Evidence is `hardware-inventory/x11-array-compression-full` and `hardware-inventory/framebuffer-msaa-isolated` on boot `9cec5575-7a22-43f2-80a3-0c50923f0954`.

The isolated QPA starts with blitting a multisampled `GL_R8` texture to an ordinary `GL_R8` texture. It records four color clears on the multisample source, initializes and verifies the destination, then aborts as the blit flushes the pending source batch. The first unsupported operation is not a GPU memory fault and does not depend on earlier transform-feedback or shadow tests.

Relevant implementation:

- `src/asahi/lib/agx_tilebuffer.c`, `agx_g17p_rt0_tile_layout_supported`: permits 1/2/4 samples, but for multisampling admits only R8G8B8A8/B8G8R8A8/R8G8B8X8/B8G8R8X8 UNORM. R8 MSAA is rejected before background shader construction.
- `src/gallium/drivers/asahi/agx_state.c`, `agx_g17p_direct_resolve_surface`: requires a matching single-sample `pipe_framebuffer_state::resolve` target. BG/EOT uses this path for multisample batches. Gallium does not currently set `agx_bg_eot_key::multisample_store`.
- `src/asahi/lib/agx_bg_eot.c`: already distinguishes resolve from unresolved multisample storage through `key->multisample_store`. G17P uses separate native IMGWBLK builtins for these operations.
- `src/asahi/vulkan/hk_cmd_draw.c`: has explicit selection and validation for unresolved multisample store and reload. Read those contracts and the associated PBE/texture descriptor handling before adapting them to Gallium.

Gallium candidate `fcde09144b8f8875703544dbebb862338450c10b2a5fb730608f6c05c33c64a5` now packs 2x/4x non-array Gen9 PBE descriptors through the shared codec, selects the existing multisample EOT builtin for persistent/partial stores, and reloads the selected surface. It also admits R8/RG8 UNORM layouts that share the existing eight-byte physical sample allocation. CPU tests pass; the native PBE regression failed before this addition at the unsupported pack call. The broader Asahi/poly result remains 264 passes, two expected failures and one pre-existing robust-index reference mismatch.

The exact official isolated color case now completes with Fail instead of aborting, with no GPU fault (`hardware-inventory/framebuffer-msaa-store`). Its first R8 resolve returns zero at a pixel expected to have a nonzero red channel. Full run `hardware-inventory/x11-msaa-store-full` reaches 866 selected executions (799 Pass, 63 Fail, 3 InternalError, 1 Incomplete, 43,035 NotRun, plus 15 separate configuration Pass) and stops at the depth configuration case. The depth-only framebuffer is incorrectly rejected by the color-format multisample gate even though it has zero color tile storage; a correction and regression are in progress.

The Honeykrisp implementation reference itself documents all-zero hardware readback from persistent MSAA stores. It must not be described as proven functionality. A diagnostic CPU observation of the retired R8 allocation and exact PBE logging are being added to distinguish missing store data from incorrect sample fetch/addressing. No test expectations or capabilities have been overridden.

Other known failures remain independent: clip-distance coverage reports `gl_MaxClipDistances=0` where CTS expects 8; transform-feedback interleaved paths hit backend encoding/link errors; depth/shadow repeat comparisons fail. The new array compression fix adds two passing selected executions but does not resolve those groups.

## Latest hardware evidence

The depth-only fix preserves the allocator's actual eight-byte minimum slot for MSAA; an initial regression expecting zero exposed that mismatch before hardware testing. The corrected libasahi test passes. Full run `x11-msaa-depth-full` reaches the final selected case of the first main session: 879 selected executions, 807 Pass, 68 Fail, 3 InternalError, 1 Incomplete, 43,022 NotRun, plus 15 configuration Pass. It stops at `KHR-GL30.draw_buffers.draw_buffers_1`. This is still not a complete main session.

CPU observation on the exact isolated R8 case shows:

```
base=0x5fffe74000 size=0x4000 samples=4
PBE words: 3f000024 01000fc0 fffe7400 00000005 00000000 00000000 00000000 00000000
nonzero=2048 full=2048 first=0x1000 last=0x2ffc
quarters=0,1024,1024,0 residues4=2048,0,0,0
```

Evidence progresses from `framebuffer-msaa-observe` through `framebuffer-msaa-span` to `framebuffer-msaa-residues`. All finish Fail with no GPU fault. Only byte offsets divisible by four contain nonzero data, consistent with only one of four interleaved R8 samples being populated. This is stronger than the previous all-zero resolve observation, but does not yet distinguish fragment coverage/tile-store behavior from EOT sample handling. Do not change native coverage-bit semantics without checking the corresponding captured instruction contract and proving the result on hardware.

`framebuffer-msaa-launch` additionally records exact auxiliary shader bytes via `HK_G17P_LAUNCH_TRACE=1`; the diagnostic supervisor now enables that logging only for trace-mode diagnostics. The EOT selected the 68-byte native multisample builtin ending in `28720000`, distinct from its resolving form. Normal full runs enable neither launch tracing nor CPU observation.

Next source points: `agx_nir_lower_g17p_tile_samples` in `agx_nir_prolog_epilog.c` uses native SR39 coverage; `agx_compile.c` distinguishes API sample-mask bits 4..7 from the raw tile-access operand; `agx_update_fs` sets the epilog sample count from the framebuffer. These comments and CPU regressions are not substitutes for proving all sample values survive the complete hardware path.

`draw-buffers-isolated` independently reproduces the final assertion without a GPU fault. The unmodified CTS creates up to eight color attachments, clears all of them, disables draw-buffer slot 1, draws to the others and checks the disabled attachment remains unchanged. Current native BG/EOT only handles RT0 and two specific dual-target layouts. This requires general multiple-render-target handling, including sparse slots; lowering advertised limits or skipping the official case would not fulfill the goal.

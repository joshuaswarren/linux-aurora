# Desktop GL sparse-mask depth qualification

Source commit: `67e5e2a1210` in `mesa-j700-agx-sync-20260916`.
This adds a diagnostic and a software control, not production driver code.
The isolated candidate includes cache-identity fix `b79a17bbe78` as an ancestor.

The coverage-hoisting helper changed in `5aa3f3324a0` is currently called by
the Vulkan epilog path. The sparse colour transaction packer is shared, but
the Gallium/OpenGL route needs its own execution evidence. This diagnostic
therefore requests desktop GL 3.3 and uses GLSL 330, API sample masks, discard,
fragment depth and per-sample texture reads. It is not a GLES test and does not
replace the official GL 3.3 CTS campaign.

For each exact 2x/4x sample count, it checks every API mask including zero,
both checkerboard discard phases, and two depth passes. The first pass writes
red and depth 0.25/0.375 to enabled, surviving samples. The second draws green
at depth 0.5 with all samples enabled. Previously written samples must remain
red at their original depth; the others must become green at depth 0.5.
Each inspection fetches colour and depth from every individual sample into one
wide float image, so correctness does not depend on a colour/depth resolve.
There is one readback per inspection pass and no explicit memory barrier.

## Software validation

Renderer: llvmpipe, LLVM 21.1.8, Mesa 26.0.8-1ubuntu0.3, GL 4.5 Core context
servicing the GL 3.3 request. All 16 masks at 4x passed both discard phases and
both depth inspections: **65,536 sample observations**. A 2x request was rounded
to 4x; it is explicitly reported as untested, not credited as 2x coverage.
The existing resolve mode still passes for 27 formats; RGB9_E5 is reported
unsupported. No Neo or other physical GPU was exercised.

- `sparse-depth-software-control.log`
- `sparse-depth-existing-resolve-control.log`
- `sparse-depth-control-build.log`
- New Meson test: `asahi-g17p-gl-msaa-sparse-depth-software-control`.

## Hardware gate and staged artifact

Wait for the current full campaign's terminal receipt, strict accounting,
official verdict and fault scan. Do not run this diagnostic concurrently with
CTS or use it to justify interrupting the full run.

Staged aarch64 executable:
`agx-sync-20260916-linux/asahi-g17p-gl-msaa-resolve`

SHA-256:
`dce085e0274f05b95cd15f756b4edb4a2a24b5f0c2d96219dd7f1a919907bb75`.

Run its `--sparse-depth` mode first against the qualified baseline, then against
the isolated Mesa candidate using the guarded installation workflow. Preserve
the printed renderer/version, exact supported sample counts and sample totals.
A no-op DRM shim is explicitly rejected, since it cannot verify pixels/depth.
Use existing kernel-fault and library-identity checks for each hardware run.

Official follow-up case families include the 18 `KHR-GL33.shaders.fragdepth`
cases, two multisampled-to-single-sampled framebuffer blit cases, one
`KHR-GL33.frag_coord_conventions.multisample`, and the 218 cull-distance cases
covering the retained cache fix. These 239 cases are a focused regression set,
not a substitute for complete candidate qualification.

## Guarded hardware harness

`fast-iteration.py --sparse-depth-probe --profile gl33` deploys the staged
executable and runs it through the existing serialized worker/supervisor.
The host and remote locks, exact J700/boot identity, installed Mesa digest,
loaded kernel note, healthy Xorg identity, and before/during/after fault capture
are retained. The binary SHA-256 travels in the job and receipt; the supervisor
verifies it, copies it into the run directory, and executes that immutable copy.
The harness records elapsed time and the complete diagnostic log.

`cts_probe.py` verifies the renderer, each tested or rounded sample count, every
API mask, both discard phases/depth passes, and terminal observation totals.
At SIZE=16, complete 4x coverage is 65,536 observations; complete exact 2x plus
4x is 73,728. Rounded counts remain explicitly untested. A software renderer,
missing/duplicate terminal summary, incomplete accounting, graphics error,
nonzero process status, or kernel fault cannot yield a passing hardware result.
The diagnostic receipt always sets `conformance_passed=false`.

`test-cts-probe.py` covers the real software-control log and negative controls,
plus six actual supervisor argument-validation paths (stopping before all
hardware access). Fourteen tests pass; log: `sparse-depth-harness-test.log`.
These tests qualify result handling, not execution of the hardware harness.

After the live campaign terminates and its complete evidence is collected:

```sh
python3 fast-iteration.py sparse-depth-baseline-20260916 \
  --profile gl33 --sparse-depth-probe --shader-cache isolated
python3 fast-iteration.py sparse-depth-candidate-20260916 \
  --profile gl33 --sparse-depth-probe --shader-cache isolated \
  --candidate agx-sync-20260916-linux/libgallium-candidate.so
```

These are prepared commands, not completed hardware results. The candidate
installation still requires the existing guard to prove the native library is
unmapped and the GPU is healthy. Compare exact sample coverage and identities
before interpreting timing; this short diagnostic does not establish overall
driver performance parity. No hardware script was uploaded during the live CTS
campaign, and the current running supervisor retains its original inputs.

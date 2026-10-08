# Uniform publication probe for preamble qualification

`probes/preamble-uniforms.c` is an additional diagnostic, not part of the
official CTS and not evidence of Neo preamble support yet. It uses only desktop
OpenGL 3.3 APIs and GLSL 330. It requests a core context and checks the renderer
against a required command-line substring to avoid silently testing a fallback.

## Workloads and reference

Twelve workloads combine vertex-only, fragment-only, and both-stage uniform
calculations; one-input and 32-input multiply/XOR chains; and direct uniforms
versus std140 uniform buffers. Workload names are `vs-short-direct`,
`fs-short-direct`, `both-short-direct`, `vs-long-direct`, `fs-long-direct`,
`both-long-direct`, and the same six names ending in `ubo`.

Each round draws 64 independent tiles into an R32UI framebuffer, changing input
data for every draw and stage. The UBO path binds distinct aligned ranges and
orphans the allocation between rounds. The direct path updates the uniforms
before each draw. There are no per-draw fences, explicit flushes, glFinish calls,
or memory barriers. One readback follows all 64 draws. Every pixel is compared
with an independent CPU uint32 reference, including its x/y coordinates; there
are no floating-point tolerances or image-derived expected values.

Default execution is eight rounds per workload: 6,144 draws and 98,304 pixel
comparisons. `elapsed_ms` includes submission, possible lazy compilation,
readback, and CPU validation. It is diagnostic wall time, not a GPU benchmark.

## Software validation, 2026-09-15 EDT

`python3 verify-preamble-uniforms.py` builds in a temporary directory in the
`neo-builder` Linux VM with `-std=c11 -O2 -Wall -Wextra -Werror`. It uses the
system Mesa libraries, surfaceless EGL, forced software rendering, and a
disabled shader cache. It neither installs libraries nor touches the Neo.

All 12 workloads passed on llvmpipe (LLVM 21.1.8, 128 bits), Mesa
26.0.8-1ubuntu0.3, OpenGL 4.5 core context. The program itself remains GL 3.3.
Two deliberately broken copies were then compiled only in that temporary
directory. Reusing draw zero's direct uniforms and binding UBO range zero for
every draw both failed as expected at round 0, draw 1, pixel (4,0): expected
`56bfa5cd`, actual `2f4351a2`. Both returned 1. This demonstrates sensitivity
to stale input publication instead of merely showing an all-green baseline.

`preamble-uniforms-controls.log` records all three results and source SHA-256
`e665086928f9a49e05dfb33e8287d4774fe06819c77e41927ac95171c385aed7`.
The earlier independent full software run is `preamble-uniforms-software.log`.
The compiled AArch64 probe is saved as `probes/preamble-uniforms`, SHA-256
`3308306343b45393ec8cb8128ba42c1046986b10e725238456cfb9f128f3dd1c`.
It has not been copied to or executed on the Neo.

## Hardware acceptance still pending

Do not run this concurrently with the live full CTS. Preserve the current Neo
Mesa and kernel while that campaign is active. The built preamble candidates
still globally disable G17P preamble generation, so merely running this probe
on one of those binaries would not qualify preambles.

For a later isolated, explicitly enabled candidate, first capture the exact
compiler metadata for each shader: preamble presence, byte extent, register
count, scratch requirement, and inline/external selection. A 32-input workload
is not proof that its preamble exceeds 256 bytes. Test one small stage-specific
case first, then long cases and the complete matrix, comparing preamble-on and
preamble-off results on the same candidate with caches disabled in both runs.
The existing compiler switch is `AGX_MESA_DEBUG=nopreamble`; it only disables
formation and cannot override the current G17P `no_preamble` key.

The vertex launch-count audit in `PREAMBLE-UPLOAD-AUDIT.md` remains a runtime
prerequisite. Candidate `6743d7ad6c1` now encodes proven native thread-invariant
IPR/spill counts and removes the legacy AGX2 resource fields; it is not installed.
Qualify the changed vertex words with preambles disabled first. Then resolve
the constant-program entry contract, including native `cprog_uniform_inputs`
and the input-count-dependent TPS footprint, before a preamble-on hardware probe.
VsStateConfig flag meaning and nonzero preamble IPR still need qualification.

`agx_disk_cache_init()` disables the disk cache whenever any compiler debug flag
is set, including `shaders`, `shaderdb`, or `nopreamble`. Therefore a logging
run cannot establish warm disk-cache behavior. Qualify that separately with
debug flags absent, an isolated cache directory, and independently confirmed
cache hits. The source's original-BO mapping and batch lifetime must remain
valid on cache restore.

These GL tests do not cover compute preambles, geometry/tessellation lowering,
all implicit entry state, or M1/M2 performance parity. Those checks and affected
official CTS families remain required before general enablement.

`PREAMBLE-INPUT-ABI-AUDIT.md` now supplies an encoder/compiled-code cross-check
for packed TPS inputs, with 17 captures and 86 exact entry reloads. This narrows
the entry-contract work; it does not qualify these GL probes on Neo or establish
the external-entry prelude, generated-uniform publication, or stage flags.

# G17P adjacency restart — 2026-09-15

The official `KHR-GL33.primitive_restart.restart_mode` failure is fixed on the
Neo. The full GL 3.3 goal remains active. The previous full-sweep measurement
was 22 non-passes in 9,886 executions; this targeted fix does not constitute
a new full sweep or official multi-configuration qualification.

Mesa branch `j700`, commit `8326b87458c` (local, not pushed).

## Cause and fix

The driver expanded primitive restart through the existing Poly kernel, but
the AGX3 ballot encoder always marked a register-comparison input as a final
read. In `poly_work_group_first_true`, the input to the second ballot is still
needed by the following broadcast. Hardware destroyed that input, so the
selected ballot was zero and its first-set-bit position became 32. Restart
tokens consequently remained in the expanded index list.

The input ballot and its shared-memory copy were correct. Examples
`0xffffff00`, `0xfffffe01`, `0xfffffff0`, and `0xfffff000` incorrectly returned
32. The correction preserves the source, and those same inputs return 8, 0,
4, and 12. The production kernel and draw path are unchanged.

`agx3_subgroup_core.c` now selects source field 25 for a live register operand
and field 27 for its final read. The paired signed-ballot kernels compiled
with Apple's Metal 32023.917 for `applegpu_g17p` produce instructions
`17075400020000582206` (dead) and `17075404030002182206` (live). The encoder
reproduces both exactly. A regression test and reproducible MSL/pipeline
script are included in the Mesa worktree.

This changes one source-lifetime field selection. It adds no instructions,
barriers, waits, or draw calls, and leaves the M1/M2 path unchanged.

## Hardware and regression evidence

Final production library SHA-256:
`9cc5baf5878c7c0b7a1fe8491dc32d7b7074443f08c5421645b600e5db33be49`.

- Official restart case: **Pass**, receipt under
  `fast-iterations/restart-ballot-live-production-20260915/`.
- Cull-distance functional family: **160 Pass**, under
  `fast-iterations/restart-ballot-live-cull-regression-20260915/`.
- Transform-feedback family: **21 Pass**, under
  `fast-iterations/restart-ballot-live-xfb-regression-20260915/`.
- Pipeline-statistics family: **8 Pass, 1 NotSupported, 1 Fail**. The
  unsupported case requires GL 4.0 tessellation; the failure is the already
  known `functional_fragment_shader_invocations` query issue. See
  `fast-iterations/restart-ballot-live-pipeline-regression-20260915/`.
- The adjacency probe produces **34 identical result rows** on Asahi and
  llvmpipe. It covers direct/indirect draws, leading/middle/trailing restart
  tokens, and segments crossing subgroup and 1,024-thread workgroup bounds.
  Logs: `restart-ballot-live-boundaries{,-llvmpipe}.log`.
- Compiler tests: **476 passed**, one disabled, in
  `restart-ballot-live-compiler-tests.log`.
- Rebuilt Asahi/Poly tests: **290 OK, 2 expected failures, 3 known failures**
  (`asahi-g17p-precompiled-fill`, `asahi-g17p-mrt-mixed`, and
  `hk_agx3_ioto_program_core`). No golden fixtures were repinned.
  Log: `restart-ballot-live-asahi-poly-rebuilt-tests.log`.
- Readback remains unresolved: **92/2,988 first-read mismatches**, all 92
  second reads correct, in `restart-ballot-live-readback-regression.log`.
  This single run does not establish a change in the failure rate.

## Diagnostic accounting

Diagnostic copies live under `probes/`, outside the Mesa source worktree.
The temporary `agx_state.c`, `geometry.cl`, and `asahi_clc.c` replacements in
the builder were restored before the production build. No observer,
diagnostic early return, or diagnostic kernel is installed in the final
library.

`restart-dump-install-20260915` successfully installed its diagnostic library
but selected a nonexistent CTS case; its QPA contains zero results. It is
**not a passed smoke test**. Subsequent installation checks used the real
`KHR-GL30.texture_lod_basic.lod_selection` case and recorded Pass.

An intermediate production experiment omitted the discard field and already
passed restart (library `e955b43f…`). The final implementation also selects
Apple's witnessed live-source field and was separately retested; its digest
is the one listed above. Diagnostic ballot logs demonstrate the destructive
read before the change and preservation after it; they are not CTS runs.

Kernel source provenance is corrected in `FENCE-VISIBILITY-AUDIT.md`:
`neo-encoder-linux` at `c85eec3bf95b22b15f87bedf5cb29717b01586f8` plus the
existing MSAA patch matches the running module. No kernel, resident, reboot,
partition, or other-computer change was made for this fix.

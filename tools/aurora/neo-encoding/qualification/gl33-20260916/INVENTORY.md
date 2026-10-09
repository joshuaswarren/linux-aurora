# Official GL3.0 run inventory

Pinned CTS `opengl-cts-4.6.8.1`, commit `067e8832315e79817ede1c4863804e440f5d1c80`, unmodified upstream surfaceless target. Native Neo J700 discovery reports Mesa / Apple A18 Pro GPU / OpenGL 4.6 Compatibility Profile, without version overrides or Zink. The advertised version is not proof of conformance.

On boot `59b80248-af05-431c-b5df-42a7870b27eb`, the official command `cts-runner --type=gl30 --summary` generated six sessions:

1. `CTS-Configs.*` metadata session.
2. The official GL3.0 no-context list: one case at 64x64, seed 1.
3. The official GL3.0 main list: 878 cases at 64x64, seed 1.
4. The same 878 cases at 113x47, seed 2.
5. The same 878 cases with the rgba8888d24s8 FBO, width 64 and automatic height, seed 3.
6. The same 878 cases with the rgba8888d24s8 FBO, automatic width and height 64, seed 3.

That is 3,513 selected case executions plus the configuration metadata cases. The actual full-run QPA/summary must confirm all sessions and their complete results. No filters or waivers were supplied. The surfaceless platform exposes a single default configuration to the runner; this inventory is not an EGL/GLX window-system qualification.

The summary-only command exits 1 and prints `0/6 sessions passed` because upstream `PRINT_SUMMARY` skips `iterateSession()` while retaining session accounting (`glcTestRunner.cpp:818-830`). Its XML has Type="gl30" and intentionally no Conformant attribute. This is an inventory, not a failing or passing CTS execution.

Upstream full-run session success requires `numFailed == 0 && isComplete` (`glcTestRunner.cpp:914-927`); NotSupported is explicitly accepted by the runner. Preserve and report NotSupported, warnings, waived, device-lost and missing cases separately rather than renaming them passes. A final claim also requires all expected sessions/cases, no waivers or hidden applicable gaps, and healthy kernel evidence.

Preparation initially missed the private Waffle shared-library directory, preventing the renderer probe from starting. That attempt is retained under `hardware-inventory/inventory`. The corrected probe and official inventory are under `hardware-inventory/inventory-waffle`; no GPU fault or kernel-stream overrun was detected.

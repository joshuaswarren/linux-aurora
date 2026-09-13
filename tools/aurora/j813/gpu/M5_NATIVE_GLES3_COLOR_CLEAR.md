# Native M5 GLES 3.0 color-clear qualification

All 19 `dEQP-GLES3.functional.color_clear.*` cases passed on physical M5
hardware on 2026-09-13. The prior driver passed 11 and failed eight masked or
complex cases. Correct HAL300 translucent fragment-pass encoding fixes those
eight failures; shader code and Khronos comparisons are unchanged by that fix.

- Mesa `j813`: `279eba20454982657263543422f79ab82b0164f9`.
  Its parent `98af8c35c9c2645ec8348b6f101aa1680c0cf030` preserves half constants
  across parallel shader exports, validated by 419 compiler regressions.
- Driver SHA256: `e040883355c01736c72a3acf9da06e1956c355cd93dfc109c6f89658b536014b`.
- Unmodified CTS source: `1942832354c96f0de379f491519734256fbd4f9d`.
- Native renderer: `Apple M5 (G17G C0)`, Asahi Gallium, surfaceless EGL,
  64x64 pbuffer, `rgba8888d0s0ms0`, base seed 1.
- Kernel Image SHA256: `90769960880b5f563aa00638914efed27918654d1008606e9da83808532c588e`.
  Headless GPU/thermal/N1 image, one P core, WFI, CPU cap 1.308 GHz.
- 19 cases started and completed, no missing/unexpected cases, 278 unique GPU
  generations with both queues retired, no driver rejection or kernel fault.
- Runtime 7.948 seconds. Highest reported sensor reading sampled once per
  second ranged from 43.709 to 43.840 C. The 70 C abort guard stayed active;
  no thermal abort or timeout occurred.

The adjacent JSON retains runner configuration, native library provenance,
driver digest, real temperature samples and result counts. The adjacent QPA
retains the unmodified Khronos case results. QPA SHA256:
`2691bfcccd3a2632c13eaf00a1b76c097966c51cfb1079ba1dad67b81b9148af`.

This is one passing test group, not GLES 3.0 conformance. The complete inventory
has 54,799 cases; 54,780 remain unexecuted. Final qualification must cover the
complete required set and configurations on the final driver. Full desktop
thermal integration and sustained ten-core thermal validation remain pending.

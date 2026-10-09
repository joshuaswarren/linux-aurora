# Gen9 array texture compression selector

The unmodified official case `KHR-GL30.ext_texture_shadow_lod.texture.sampler2darrayshadow_fragment` independently faults on clean boot `aa858c75-0d2c-4033-95c3-938840b9d738`. The native Mesa library was `ef8f21e5b82b3a955d5c72d708490671ab4284c71c652858bf86aa6831c213ec`. This diagnostic reproduction is separate from the full official run and does not add suite coverage.

Evidence in `hardware-inventory/shadow-array-isolated` includes the receipt, original QPA, kernel log, resource/command trace and derived `texture-analysis.json`. Two identical Z16 descriptors describe a 128×128 four-layer array with base `0x5ffff20000`, layer stride `0x8000`, allocation size `0x20000` and secondary address `0x5ffff40000`. The semantic compression state is false, while wire bit 59 is true:

```
f0000263 0801fc07 ffff2000 c000c005 ffff4000 00000005 00000000 00000000
```

The GPU reads `0x5ffff400c0`, 0xc0 past the allocation end. The earlier 2D correction left the forced compression selector active for other extended dimensions, so the same mismatch remains in arrays independently of mipmapping or prior CTS cases.

The correction removes the forced selector for every Gen9 dimension and uses the existing semantic compression field. Native cube and array capture bytes remain unchanged; their test inputs now represent the compression bit present in those captures. The cube payload guard still requires the extended form and secondary address. Driver-level compressed-resource support is unchanged.

The new `test_gen9_uncompressed_dimensions` covers array, 3D and cube views. Before the correction it fails specifically at `!decoded.compressed` (`uncompressed-dimensions-before.log`). After the correction the native descriptor test passes. The complete Asahi/poly regression set reports 264 passes, two expected failures, and the same pre-existing robust-index size/hash mismatch in `asahi-g17p-precompiled-fill`. Captured reference bytes were not regenerated. `git diff --check` passes.

Hardware validation: on fresh boot `9cec5575-7a22-43f2-80a3-0c50923f0954`, candidate `b4830bba7ad5e16f353c957903c5e3df53aeb0855108e64b62ae4db1c9958968` completes the same official case with Pass, exit 0 and no GPU fault. Evidence is `hardware-inventory/shadow-array-compression-fix`. The first two descriptors have identical addresses and all eight words match the faulting run except word 1 XOR `0x08000000` (compression bit 59). The successful run continues and records six texture descriptors overall.

This validates the isolated shadow-array case and removes its demonstrated fault. It does not establish every array/3D/cube path or full GL3.0 conformance. The full official runner restarted without diagnostic flags or case filtering.

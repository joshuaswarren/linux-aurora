# GL 3.3 NotSupported audit — in progress, 2026-09-15

This records applicability checks against the exact tested CTS commit
`067e8832315e79817ede1c4863804e440f5d1c80`. It does not turn NotSupported into
Pass or justify every skip in a future full run. Each completed campaign must
still account for all cases and configuration groups.

The first uncached generation-fix campaign showed these reasons before it was
deliberately stopped to qualify a faster shader-cache policy:

| Observed class | Source condition | Qualification consequence |
| --- | --- | --- |
| `CommonBugs.CommonBug_PerVertexValidation` | Test reports it is implemented only for GL 4.1+ contexts. | Does not establish missing GL 3.3 functionality. Preserve as a version-limited test. |
| `packed_depth_stencil.stencil_texturing.*` | Despite its extension-related message, the test checks only whether the requested desktop context type is at least GL 4.3. It does not query `GL_ARB_stencil_texturing`. | A version-limited branch in the GL33 suite; not proof that the advertised extension is missing. |
| `shaders.fragdepth.write.*` and `compare.*` | `renderTarget.getDepthBits() == 0` throws NotSupported. | These must be exercised on a depth-bearing configuration elsewhere in the complete matrix. A zero-depth configuration is not proof of absent depth support. |
| `shaders.shader_integer_mix.prototypes` | This registered case passes `use_extension=false` and uses the suite's GLSL version. Desktop core prototypes require GLSL 4.50, so the GL33/GLSL330 variant skips. | The applicable `prototypes-extension`, `define`, negative and ivec/uvec/bvec functional cases all passed. Do not mislabel this as missing advertised integer-mix support. |
| `pipeline_statistics_query_tests_ARB.functional_tess_queries` | Test reports GL 4.0+ required. | Keep version-limited status distinct from GL 3.3 failures. |
| `cull_distance.functional_test_item_8_primitive_mode_lines/triangles_*` | Item 8 sets `fetch_culldistances=true`; the test explicitly skips that mode for primitives other than points (line 2915). It does not request passthrough tessellation or geometry. | These are excluded test combinations, not evidence that the driver lacks tessellation or cull-distance support. Require the corresponding applicable point and other cull tests to pass. |
| `texture_swizzle.smoke_access_idx_1_channel_idx_*` | Smoke target is `GL_TEXTURE_2D_ARRAY`; access 1 is `textureProj`. `isTargetSuppByAccess` rejects projective access to a non-projective target. | The test skips an invalid sampler/target combination after checking the texture target is supported. Do not misclassify its generic "Target not supported" message as missing 2D-array textures. |

The actual CTS info cases report Apple A18 Pro, GL 4.6 Core / GLSL 4.60 and
advertise integer-mix, stencil-texturing and cull-distance extensions. The CTS
profile selector still gives the above tests their GL33/GLSL330 test metadata;
the exact source predicates explain those apparent discrepancies. This work
qualifies the GL33 test set, not all features of the advertised GL 4.6 API.

Primary source references:

- [Fragment-depth configuration gate](https://github.com/KhronosGroup/VK-GL-CTS/blob/067e8832315e79817ede1c4863804e440f5d1c80/external/openglcts/modules/common/glcFragDepthTests.cpp).
- [Integer mix extension gate](https://github.com/KhronosGroup/VK-GL-CTS/blob/067e8832315e79817ede1c4863804e440f5d1c80/external/openglcts/modules/common/glcShaderIntegerMixTests.cpp).
- [Packed depth/stencil version gate](https://github.com/KhronosGroup/VK-GL-CTS/blob/067e8832315e79817ede1c4863804e440f5d1c80/external/openglcts/modules/common/glcPackedDepthStencilTests.cpp).
- [Cull-distance item table and applicability checks](https://github.com/KhronosGroup/VK-GL-CTS/blob/067e8832315e79817ede1c4863804e440f5d1c80/external/openglcts/modules/common/glcCullDistance.cpp).
- [Texture-swizzle access/target checks](https://github.com/KhronosGroup/VK-GL-CTS/blob/067e8832315e79817ede1c4863804e440f5d1c80/external/openglcts/modules/gl/gl3cTextureSwizzleTests.cpp).

## Completed first-configuration skip audit

The completed first configuration of the older, terminal
`readback-generation-full-cached-gl33-20260915` campaign contains 307
NotSupported results. Its main QPA SHA-256 is
`ad84e497b8fd4454ca056feeb2d7b7eb04a4a2c1093e8122d680a8261df752e8`.
These results remain separate from the new fixed-Mesa full campaign.

The texture-swizzle source at the pinned CTS revision has SHA-256
`5e340f6eb7d9e85e9b56a4120e8261aa1e28a5a1d3d35287764b5a040f09552e`.
Its table and predicates account for all 263 swizzle skips:

| Count | Exact applicability reason |
| --- | --- |
| 99 | Eleven format-table entries require GL 4.2, 4.3, or 4.4 metadata; each is tried against nine targets. Indices: 8, 9, 10, 13, 14, 17, 18, 19, 24, 69, 70. |
| 120 | The functional test fixes accessor index 4 (`texelFetch`). The source excludes rectangle targets for this LOD-taking accessor and cube targets for integral-coordinate fetch; 60 format entries each. |
| 24 | Six projective accessors (1, 6, 8, 9, 12, 13), each with four channels, cannot be used with the smoke test's 2D-array target. |
| 18 | Six depth/depth-stencil format entries (63–68) are explicitly excluded for 3D, 2D multisample, and multisample-array targets. |
| 2 | `GL_RGB9_E5` is explicitly excluded for the two multisample targets. |

A predicate cross-check of all 695 functional/smoke results found zero
applicability mismatches: 432 applicable cases and the 263 exclusions above.
The two additional swizzle cases are outside that 695-case cross-check.
The minimum-version gate belongs to individual CTS table entries; this does
not mean every occurrence of those underlying GL formats requires that version.

The remaining 44 skips are the previously audited 18 depth-buffer-dependent
tests, 16 nonpoint cull-distance item-8 combinations, two stencil-texturing
version gates, one integer-mix prototype version gate, one GL4.1 common-bug
test, one GL4.0 tessellation query test, and five texture-filter-minmax tests.
For minmax, the recorded renderer does not advertise
`GL_ARB_texture_filter_minmax`, and the
[pinned test source](https://github.com/KhronosGroup/VK-GL-CTS/blob/067e8832315e79817ede1c4863804e440f5d1c80/external/openglcts/modules/gl/gl3cTextureFilterMinmaxTests.cpp)
checks for that extension before the affected tests. It is an explicit absent
extension result, not a pass.

Current fixed-build evidence: all 18 `KHR-GL33.shaders.fragdepth` cases have
complete Pass results in
`config-gl33-main-cfg-1-run-34-width-64-height--1-seed-3-egl.qpa` of
`cache-identity-full-gl33-20260915`. Strict XML parsing confirms their statuses.
They remain NotSupported in the two preceding fixed-size sessions. This closes
the earlier absence of any executed fragment-depth cases in this campaign; it
does not qualify the remaining configurations or explain the render-target
selection difference by itself.

Outstanding: finish the fixed-build campaign and audit any new skip classes;
verify mandatory depth/format functionality across the remaining configurations;
check advertised extensions against the observed extension-dependent results.
No skip count is added to the Pass total.

## Repeatable current-campaign review, 2026-09-16

`audit-gl33-applicability.py` reads completed main QPAs without modifying them.
It pins the release identity above, checks the complete 9,886-case inventory,
classifies only the reviewed exact skip conditions, and rejects unexplained
NotSupported results. It compares every functional/smoke swizzle result with
the table predicates, rejects depth skips on a depth-bearing configuration,
and rejects the optional-minmax explanation if that extension is advertised.
It preserves Pass and NotSupported as separate counters. Its result is an
applicability review, not an official conformance verdict or proof of campaign
completeness; `analyze-run.py` remains the independent final accounting gate.

Two immutable logs from `cache-identity-full-gl33-20260915` were copied read-only:

| Local copy | Main results | SHA-256 |
| --- | --- | --- |
| `unsupported-audit-cfg1-window.qpa` (`cfg-1-run-32`, 64x64, seed 1) | 9,579 Pass / 307 NotSupported | `af14d018afc20b9034961af211378a0dee7b4a57756a6fecde4f7de5726cf9d5` |
| `unsupported-audit-cfg1-pbuffer.qpa` (`cfg-1-run-34`, 64x-1, seed 3) | 9,597 Pass / 289 NotSupported | `a08bf12bd048eb5dbd71db5cdc5721de10e0f8eea41dd29bf3af9b58bb402c99` |

The report `gl33-applicability-current.json` has zero unknown skip classes or
applicability issues across 19,772 executions. All 1,390 swizzle predicate checks
agree. Both configurations have 6 Pass / 1 metadata-limited NotSupported for
integer mix and 202 Pass / 16 excluded combinations for cull distance. The
depthless window skips 18 fragment-depth tests; the pbuffer has 24 depth bits
and passes all 18. Reported GL4.6 and advertised integer-mix/stencil-texturing
remain consistent with the already-audited GL33/GLSL330 metadata gates.

`test-gl33-applicability.py` passes seven controls using temporary copies:
the two actual configurations, an injected skip for a required swizzle case,
an advertised-but-skipped minmax extension, a depth-bearing target with skipped
depth tests, a changed CTS revision, and an unexplained new skip. Original logs
are preserved. Output: `applicability-regression-test.log`.

After final collection, run:

```sh
python3 audit-gl33-applicability.py /absolute/path/to/collected/run \
  --output /absolute/path/to/gl33-applicability-final.json
```

The two-file review does not qualify all 50 configurations. The final review
must use all collected main logs together with the official verdict, strict
session accounting, binary identities, and hardware-fault scan.

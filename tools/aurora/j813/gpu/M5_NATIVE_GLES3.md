# Native M5 GLES 3.0 qualification

On September 13, 2026, physical J813 passed the unchanged Khronos case
`dEQP-GLES3.functional.color_clear.single_rgba` through native Asahi Gallium.
All 30 iterations completed with the original pixel thresholds. All 30 GPU
submissions retired both vertex and fragment queues; 29 batches contained draws.
No driver errors or rejected draws were recorded. This is one passing case,
not full GLES 3.0 conformance.

The source is Mesa `j813`, hardware changes in `57296534ef7`, plus the G17G
compiler/context fixes in `0d79917f8b7`, `2bba71c03ef`, and `15f6b6a7bd2`.
Khronos VK-GL-CTS was unchanged at
`1942832354c96f0de379f491519734256fbd4f9d`.
The tested kernel was `7.1.6-aurora-j813-gpu-v9`, build 15, with all ten CPUs
online. The native driver SHA256 was
`fe3fc8e42dcc497aa74240d8cdb28c58d23760a75424ec10ee1d36e4383e2a2f`.

The accompanying `.qpa` is the actual CTS result. The JSON records qualification
scope and provenance. The runner checks physical J813 identity, mapped native
driver libraries, the M5 renderer, timeout status, and complete passing results.
It exits nonzero for incomplete, failing, or unverified runs.

## Reproduction

Build native Asahi Gallium and unmodified VK-GL-CTS for the target. The runner
expects the native runtime in `/opt/m5-native-gallium` and surfaceless CTS in
`/opt/m5-cts/surfaceless`. Its `native-env` launcher must select `asahi` for both
`MESA_LOADER_DRIVER_OVERRIDE` and `GALLIUM_DRIVER`, point GL/EGL paths at that
runtime, and clear `LD_PRELOAD`, software-driver and GL-version overrides.
Use SSH/SCP over N1 Wi-Fi after the board's RAM userspace starts.

```sh
python3 run-m5-native-cts.py single-rgba \
  --binary deqp-gles3 \
  --case dEQP-GLES3.functional.color_clear.single_rgba \
  --platform surfaceless --config rgba8888d0s0ms0 --seconds 45
```

The pbuffer is 64 by 64 pixels. Results, normal CTS images, the command, loaded
library evidence, driver hash and kernel log are retained under
`/run/m5-native-cts/single-rgba`. Optional `AGX_MESA_DEBUG=shaders` and
`ASAHI_MESA_DEBUG=faulttrace` add compiler/driver diagnostics.

The current experimental kernel retains the first submitting GPU VM. Start
the test before other GPU clients and use a clean boot for a different client.
Do not count the `g17g-compile-only` DRM shim as hardware evidence: that offline
compiler profile deliberately rejects every submission.

## Resolved failures

The original VDM state fetch used an absolute PPP address where M5 expects an
offset from its GART base. The observed fault was exactly one architectural
base above the mapped state page. Sharing Honeykrisp's address conversion
removed that fault.

Subsequent fast clears and CPU readback produced correct colors, but utility
rectangle draws were refused when their vertex shader exported a color unused
by the fragment shader. Separating vertex output allocation from fragment
interpolation preserves all UVS words while emitting zero fragment bindings.
That correction allowed the complete 30-iteration case to pass. Textures,
additional draw states, and the rest of GLES 3.0 still require qualification.

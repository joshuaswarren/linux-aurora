# J813 thermal monitoring and CPU cooling

These separate RAM images expose 43 SMC temperature channels and thermal zones,
plus CPU frequency policies and cooling devices for the M5's E/P clusters.
The minimal thermal image omits GPU, DCP, Wi-Fi and MTP. The headless GPU image
adds GPU and Wi-Fi for offscreen GLES tests, with an explicit SMC handoff.
Neither image starts a graphics workload during boot. The previously qualified
desktop artifacts are not replaced.

The thermal startup script loads `apple-soc-cpufreq`, caps each online cluster
at its lowest advertised frequency, and loads `macsmc` and `macsmc-hwmon`.
It checks all 43 channels, passive trips, and cooling-device bindings before
printing `M5_THERMAL_MONITORING_AND_LOW_CLOCK_CAPS_READY`. It does not online
additional CPUs. Use WFI idle, not the earlier diagnostic `idle=nop` setting.

The device tree uses a 75 C passive trip and 5 C hysteresis for each channel.
This is a conservative bring-up policy, not Apple's factory calibration.
No critical-shutdown threshold has been invented. Labels retain SMC key names;
their individual physical locations have not been established.

## Hardware qualification, September 13, 2026

The physical J813 booted with one P core, then one E core was brought online
through the established first-time spin-table path. WFI idle remained enabled.
All 43 channels were readable; the initial Linux snapshot ranged from 36.7 to
45.3 C. These are post-cooldown readings; the earlier peak is unknown.

| Check | P cluster | E cluster |
| --- | --- | --- |
| CPU cooling device | `cpufreq-cpu0` | `cpufreq-cpu1` |
| Cooling callback | 1.608 to 1.308 GHz | 1.152 GHz to 972 MHz |
| Lowest frequency cap | 1.308 GHz | 972 MHz |
| Passive trip using real temperature | Activated | Activated |
| Restored trip | 75 C | 75 C |

Frequencies above are command-register readbacks, not independent clock
measurements. The standard cooling callback imposed a lower policy limit and
requested clock. Automatic activation was checked separately by temporarily
lowering one passive trip below the actual measured temperature. The readings
were not emulated and no stress workload was used. The script restored the
75 C trip and lowest frequency cap afterward. This does not qualify sustained
high-load thermals or the complete desktop integration.

`qualify-cooling.sh p` and `qualify-cooling.sh e` reproduce these bounded checks.
They accept only the observed minimal CPU topology and restore the low cap on
exit. `thermal-status.sh` reads temperatures, policies, zones and cooling state
without changing them.

A subsequent fresh boot of the packaged initramfs automatically reached both
the thermal-ready marker and the verified Linux shell. It exposed 43 channels
and zones, with one P core online, WFI idle, and a 1.308 GHz maximum/requested
frequency. That boot's initial readings were 36.2-44.8 C.

## Build and boot

On the existing `j813-builder` VM, run `build-candidate.sh`. It merges
`../thermal.config` into the separate thermal kernel build, uses two build jobs,
and packages `artifacts/thermal` from the minimal CPU initramfs.
Pass `gpu` to package the headless GPU image at
`artifacts/gpu/native/gpu-thermal-candidate`, including the qualified GPU
firmware and N1 userspace from the existing GPU initramfs.

On a freshly reset J813, chainload the guarded SMP carrier, then use the host
boot helper:

```sh
python boot-linux.py --thermal --smp --smp-boot-cpus=1 \
    --smp-idle=wfi --smp-no-wfxt --smp-diagnostics
```

The host helper checks J813/T8142 identity and requires one initial Linux CPU
for this thermal image. The spin-table implementation cannot offline CPUs;
do not try to reduce a CPU's hotplug target after bringing it online.

The headless variant uses `--gpu --gpu-initdata --gpu-thermal
--gpu-candidate=gpu-thermal-candidate`, the same one-CPU WFI options, and the
explicit guarded carrier build/SHA256 pair. Boot verifies GPU firmware regions
and quiesces the owned SMC RTKit session after N1 PCIe preparation. The SMC CPU
remains running. Linux then performs its normal SMC handshake. A physical boot
verified all 43 channels/zones, the low CPU cap, GPU render-node registration,
and Wi-Fi association together.

The unchanged v12 Mesa library then completed all 19 native GLES3 color-clear
cases: 11 Pass and 8 Fail, exactly matching the previous per-case results.
All 192 GPU queue pairs retired. The 5.164-second run reported real temperatures
of 43.840-44.262 C, with no timeout, thermal abort, or kernel fault. The eight
masked/complex failures still encounter the native opaque-draw profile limit.
`gles3-color-clear-qualification.json` retains the report and QPA hash.

The native CTS runner now requires 43 working SMC channels, bound cooling
devices, and low CPU clock caps. It rejects polling idle and stops at a measured
70 C by default, recording a thermal abort as failure. It samples temperatures
before, during, and after execution; Khronos pixel expectations are unchanged.
This is a test-run stop policy, not GPU DVFS or factory thermal calibration.

## Handoff constraints

Wi-Fi preparation previously retained a Python SMC session, and the subsequent
Linux SMC handshake timed out. The headless GPU path now closes that session
with standard RTKit QUIESCED transitions before Linux takes over. It requires
a freshly owned session and checks both acknowledgements and CPU RUN state.
Desktop integration still requires WFI qualification with the display and input
stack. The CPU-only and headless GPU results do not establish that integration.
Do not resume the prior ten-core polling graphics image as a thermal solution.

Reading SMC wrapper offset `0x48` caused an SError on this board. It is not a
qualified SMC status register and must not be borrowed from the GPU wrapper
layout. Normal SMC mailbox access and Linux hwmon reads are verified instead.

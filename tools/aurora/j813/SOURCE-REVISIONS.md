# J813 Linux source locations

The primary driver and workbench now live together on
[linux-aurora/j813](https://github.com/aurora-silicon/linux-aurora/tree/j813).
All repositories use the single `j813` branch for this work.

| Component | Source |
| --- | --- |
| N1 Wi-Fi driver | `drivers/net/wireless/apple/centauri/` in this kernel tree |
| Workbench and tracing tools | `tools/aurora/j813/` in this kernel tree |
| J813 device trees and AIC3 changes | This branch; kernel bring-up baseline c8f85bcfb039 |
| Native Linux m1n1 | [m1n1-aurora/j813](https://github.com/aurora-silicon/m1n1-aurora/tree/j813), repository root; qualified source be543d9e |
| macOS tracing carrier | [m1n1-aurora/j813](https://github.com/aurora-silicon/m1n1-aurora/tree/j813/experiments/j813/macos-trace), `experiments/j813/macos-trace/`; qualified source dc702bf6 |
| U-Boot | [u-boot/j813](https://github.com/aurora-silicon/u-boot/tree/j813), b0430a0846b; leaves J813 MTP cold for Linux |

## Reproducing the lab workbench

The source retains the tested lab's absolute paths. On that host, copy the
files in this directory into `/Users/mcruz/Developer/aurora-silicon/j813-linux`
without copying over its nested repositories or private data. The `linux/`
checkout must point to this branch. The lab `hv/m1n1` path must resolve to
`experiments/j813/macos-trace/` in the m1n1-aurora `j813` checkout, while the
lab's native `m1n1` path uses that checkout's root. The updated build scripts obtain the
driver from that kernel checkout; there is no second driver source copy here.

The dedicated `j813-builder` Lima VM and sibling AuroraSilicon debug runtime
remain build dependencies. Other hosts must adapt paths and controller
selection and supply their own firmware/calibration and native macOS inputs.
No private captures, firmware, generated RAM images, credentials or webhook
values are included.

Enable `CONFIG_WLAN_VENDOR_APPLE=y` and `CONFIG_APPLE_CENTAURI=m` in the
Wi-Fi kernel configuration. The module retains the same `apple_centauri`
name and runtime setup described in [README.md](README.md).

# Native J700 display-off power reference, 2026-09-16

Read-only investigation using the already installed DevMem v13 / ABI 7 on
macOS 26.5.2 build 25F84. Boot UUID
`1CFB4FD4-E49F-4599-9187-34581EFCAAB5`. Automatic system sleep remains disabled
(`SleepDisabled=1`, battery/AC `sleep=0`); display idle remains one minute.
No kernel, firmware, PMGR register, or kext was modified. No Linux reboot or
upstream fetch/push occurred.

Raw evidence is in the parent workspace's
`artifacts/neo-suspend/macos-pmgr/`; remote originals are in
`root@mac-neo:/var/tmp/neo-pmgr-native/`. The earlier ~0.13 W native awake
display-off and ~0.05 W native sleep estimates are documented separately in
`macos-sleep-baseline.md`. These register captures are not new wattage tests.

## Physical domain comparison

The live macOS ADT `devices`, `ps-groups`, and `reg` properties decode to the
same table as `j700-pmgr-domains.json`, SHA256
`67267feae9e57dbfaa513b303947f4025d129f162a1f2ce72a0d75be769d0d25`:
296 entries, 197 virtual, 99 physical. Each physical PMGR state word was read
as one aligned 32-bit access. Virtual descriptors were excluded. Both full
captures completed in approximately 0.22 s with all 99 reads successful.

`idle-01` observed 63 OFF, 6 CLKGATE, 30 ACTIVE; `idle-02` observed 64 OFF,
6 CLKGATE, 29 ACTIVE. Only two PCPU actual states and NUB_FABRIC actual state
changed between scans. Policy differences are therefore substantially more
stable than instantaneous CPU/fabric residency.

The Linux comparison is the **archived awake**
`domain-gate-review/physical-status.txt`: 18 OFF, 19 CLKGATE, 62 ACTIVE.
Do not describe this full 99-domain inventory as measured during Linux sleep.
Separate #42 `usb-clock/short-1/result.json` noirq endpoint snapshots confirm
DISP_SYS, DISP_FE, and DISP_CPU ACTIVE at both suspend/resume endpoints.
They still do not measure continuous residency inside the sleep interval.

| Domain | Linux archived word | macOS idle word | Native observation |
| --- | --- | --- | --- |
| DISP_CPU | 0x1f0000ff | 0x0f000300 | Manual OFF, AUTO disabled |
| DISP_SYS / DISP_FE | 0x000000ff | 0x00000300 | OFF |
| DISPEXT0_CPU | 0x1f0400ff | 0x0f040300 | Manual OFF, AUTO disabled |
| DISPEXT0_SYS / FE | 0x000020ff | 0x00002300 | OFF |
| SIO_CPU | 0x0f0000ff | 0x1400034f | AUTO enabled, actual CLKGATE |
| APCIE_PHY_SW | 0x1f0000ff | 0x1400024f | AUTO enabled, actual CLKGATE |
| APCIE_SYS_GP | 0x1f0020ff | 0x1000030f | AUTO enabled, actual OFF |
| AFGGFXNI0 | 0x0f0040ff | 0x1000430f | GPU-side fabric domain AUTO/OFF |
| PMC | 0x0f0000ff | 0x1400024f | AUTO enabled, actual CLKGATE |
| DEBUG_GATED / NUB_SPMI0 / NUB_SPMI1 | 0x0f0000ff | 0x1400024f | AUTO/CLKGATE |
| NUB_SRAM | 0x0f0002ff | 0x1000030f | AUTO/OFF |
| ATC0_USB_AON / COMMON / USB | ACTIVE | 0x00000300 | All three OFF |

I2C0..4, SPI0..3, QSPI, AES, PRORES, MSR, and AP_TMM also reach OFF natively.
This is not permission to raw-gate firmware-owned CPU or infrastructure
domains: previous DISP_CPU and DISPEXT0_CPU writes faulted on Linux.

Useful negative controls: GFX is `0x1000230f` on both systems (already OFF).
PMP and PMS_SRAM exactly match (`0x1f0f03ff`, `0x1f0003ff`). ANS remains ACTIVE
with the same policy; its differing 0x300 bits are sticky gating history.
Thus turning off PMP or ANS is not established as necessary to reach the
native awake floor. Firmware activity and clocks inside a matching domain
can still differ; equal PMGR words do not prove equal watts.

Bits 27:24 (AUTO_STATE) must not be treated as a write recipe. They describe
the automatic state observed by this decoder, separate from AUTO_ENABLE,
the manual target, and the minimum floor. Native `_setPSAutoMinLevel` updates
bits 19:16; copying an entire captured status word would mix policy with
hardware status and sticky flags.

## What this changes in the implementation plan

1. Follow native DCP/DCPEXT shutdown and restart, including firmware and
   ownership handshakes. The panel-off Linux result does not establish that
   its display processing domains shut down. This is now a measured gap.
2. Investigate SIO runtime ownership and automatic idle. Native SIO remains
   an attached RTBuddy service while SIO_CPU is clock-gated. Its absence from
   the Linux DT and the existing driver's suspend TODO remain relevant.
3. Investigate shared PCIe PHY **clock gating / idle requests**, rather than
   repeating the failed Linux full power-off that broke Wi-Fi. Both OSes
   already have AUTO_ENABLE set; the differing AUTO_STATE means enabling
   that bit alone will not resolve this path.
4. Audit the GPU fabric parent, PMC, Nub infrastructure and USB parent chain
   with their owning drivers. Native state supplies a target, not proof of
   an independent safe gate or a per-domain wattage.

No <0.7 W Linux result is claimed. Attribute savings with reversible driver
changes, device recovery checks, and same-session battery-counter comparisons.

## Reproduction

On this Mac, with the reviewed table alongside the script:

```
python3 macos-pmgr-snapshot.py --output NEW_DIRECTORY
```

The tool records boot/build/client identities, battery and root-domain state,
raw bytes, timing, and a durable checkpoint before each read. It pins the
table hash and hardware model and rejects virtual/unknown names. It performs
no physical RAM translator configuration. Source inspiration was the M4
workspace's `tools/audio-pmgr-snapshot.py`; no J713 address was reused.

Sampling itself runs CPUs and touches storage. These sequential reads cannot
be used as a CPU or memory-fabric residency measurement. Full IORegistry
serialization may also invoke driver code; record snapshots separately from
uninterrupted battery measurements.

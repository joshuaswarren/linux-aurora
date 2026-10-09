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

## Existing DCP history retrieved without new tracing hooks

`macos-dcp-history.py` retrieves AppleDCPExpert's existing inline trace ring
through DevMem kernel-VA reads. The matching AppleDCP binary SHA256 is
`ff6c9228d92d2d09752ad570ae031fac122ce2563960b76ee4fae3becab9ac5a`, extracted
from kernelcache SHA256
`c4de7d65a016ff68f6ac2a335517da82c9be966175c80afd8a738dd00faa1c61`.
The tool verifies the full live `_enterTrace` function and object-size
declaration against that binary, checks the registry class and vtable address,
and retains the object through one DevMem connection until all reads finish.
It does not dereference guessed object pointers or install kernel hooks.

Layout confirmed from native code:

- `_enterTrace`: unslid 0xfffffe0008d1f498, length 0xb8.
- Object size: 0x2cb8; next ring index at +0x4b0.
- Ring: +0x4b8, 256 records of 40 bytes.
- Record: mach_absolute_time u64, event u32, padding u32, three u64 args.
- Event 1: power assertion enabled/disabled, client pointer, zero.
- Event 2: old DCP state, requested DCP state, returned status.

The state-change argument mapping is established by `_changeDCPPowerStateInternal`
at 0xfffffe0008d1f6e8 and its call to `_enterTrace` at 0xfffffe0008d1f984.
Do not interpret the large count of event-2 `(4,4,0)` records as repeated
hardware shutdowns; these are successful same-state requests.

Both internal DCP (`0x10000049c`) and DCPEXT (`0x10000048c`) yielded full,
stable 256-record rings. Final entries in each are assertion releases followed
by `(old=4, requested=0, result=0)`. Corresponding current IORegistry properties:
`DCPPowerState=0`, `DCPPowerAssertionCount=0`, `DCPPowerRetained=false`.
PMGR independently shows CPU/SYS/FE OFF. The ring plus register observations
establish a successful native off transition, but **do not contain the full
firmware mailbox exchange or individual PMGR writes**.

An initial DCPEXT capture stopped at its vtable check without reading the
object: PAC bit 47 differed from the canonical address. The qualified helper
compares the low 47 address bits only, without using that masked value as a
pointer. Full-function/build/class checks remain required. Final captures
`dcp-history-03` and `dcpext-history-03` passed all checks, double-copy ring
stability, and timestamp ordering. Earlier raw captures remain retained.

Commands below use IDs from this boot's IORegistry; discover fresh IDs after
reboot instead of carrying them forward:

```
python3 macos-dcp-history.py --registry-id 0x10000049c --output NEW_DCP_DIRECTORY
python3 macos-dcp-history.py --registry-id 0x10000048c --output NEW_DCPEXT_DIRECTORY
```

This gives the next Linux DCP investigation a native transition history and
an independently checked final hardware state. It does not resolve Linux's
earlier CPU-domain write access fault; reproduce the owning driver's protocol
and access prerequisites before attempting a gate.

## Additional native residency check

Two 5-second powermetrics samples while system sleep remained disabled showed
GPU idle residency 100%, estimated GPU/ANE power 0 mW, and estimated CPU power
3 mW. E-cluster idle residency was 97.35% / 96.51%; P-cluster 99.51% / 99.71%.
Raw output is `powermetrics-idle.txt`. These are Apple's subsystem estimates,
not whole-machine power or measurements of DCP/SIO/PMP consumption. They
support a quiet CPU/GPU reference but cannot allocate the missing Linux watts.

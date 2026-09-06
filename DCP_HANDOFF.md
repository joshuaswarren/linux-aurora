# DCP handoff: m1n1 stage 2 -> Linux (J700 / T8140 / H17P)

What m1n1 does to the display coprocessor before it boots Linux, what it tells
Linux about it, and what `apple-drm` has to do with that. Written against
m1n1 `dcp-neo` (`aurora-silicon/m1n1@dcp-neo`) and macOS 25G83 traces.

## 1. What m1n1 hands over

On T8140 m1n1 does **not** pass a live DCP session to Linux. `display_shutdown()`
takes the "stopped" path (`src/display.c`):

1. Quiesce the inherited DCP without tearing down IPC.
2. Drain the inherited crashlog.
3. Stop the DCP ASC (`asc_cpu_stop_v6`), and read back `ASC_CPU_STATUS_STOPPED`
   — the stop is verified, and the handoff fails closed if it does not take.
4. Revoke the inherited DART stream. Stopping the ASC does **not** revoke its
   locked DART translation, and a Linux platform device with an `iommus`
   property would otherwise re-enable inherited stream SID 23 before any DCP
   driver binds. m1n1 resolves the SID from the ADT, disables it after the ASC
   stop, and fails closed unless the hardware enable register confirms DMA can
   no longer issue.
5. Report `DISPLAY_DCP_HANDOFF_ASC_STOPPED`.

So by the time Linux starts: the DCP firmware is not running, and its DMA path
is dead. There is no inherited state for a driver to adopt, and none to trip
over. m1n1's own log calls this a "fresh start".

That framing is the problem — see §4. macOS does the opposite: it adopts the
live iBoot generation and never rebuilds it, and a generation that has been
stopped cannot be booted fresh afterwards.

## 2. What Linux is told

m1n1 sets one state property under `/chosen`:

| `/chosen` property | state | meaning |
|---|---|---|
| `apple,dcp-rtkit-quiesced` | 1 | RTKit quiesced, ASC still running |
| `apple,dcp-asc-stopped` | 2 | **T8140 path.** ASC stopped, DART stream revoked |
| `apple,dcp-rtkit-idle` | 3 | RTKit idle |

When a DCPLink session was inherited, m1n1 additionally exports its mapping:

| property | meaning |
|---|---|
| `apple,dcp-link-rpc-dva` | RPC region device VA |
| `apple,dcp-link-rpc-pa` | RPC region physical address |
| `apple,dcp-link-rpc-size` | RPC region size |
| `apple,dcp-link-phase` | handshake phase reached |
| `apple,dcp-link-message` | last message observed |

A driver should read the state property and branch on it, rather than probing
the hardware to guess what happened before it.

## 3. Known blocker: the DCP node is disabled

In the `apple,dcp-asc-stopped` case m1n1 also calls
`kboot_disable_stopped_dcp_nodes()`, which sets `status = "disabled"` on **both**
the DCP node and its mailbox node in the FDT it hands Linux.

**Consequence: Linux never binds `apple-drm`.** The display driver cannot probe,
so nothing below this line can be exercised on Neo as things stand.

This is deliberate and enforced by
`tests/python/test_t8140_dcp_link_handoff_contract.py` on the m1n1 side, so
changing it is a contract change, not a bug fix.

**Re-enabling the node may well be the whole fix.** This driver already handles
the stopped handoff: `apple,dcp-asc-stopped` maps to
`APPLE_DCP_COPROCESSOR_ASC_STOPPED_RESTART`, for which
`apple_dcp_needs_fresh_boot()` is true, and there is a T8140-specific branch in
`dcp_probe()` that reads the IOP VBAR and resolves it through the IOMMU before
restarting the ASC. The `status = "disabled"` is what stops that code from ever
running -- the OF core never creates the platform device, so no driver-side
change can compensate.

Until this is resolved, DCP driver work on J700 can only be validated by tracing
macOS under the m1n1 hypervisor (see `MACOS_HV_AGX.md` in the m1n1 tree —
DCP tracing rides along with `--agx-full`).

## 4. macOS adopts the iBoot generation; it does not rebuild it

This is the part that determines what the handoff should look like, and it
contradicts the "stop it and let Linux start clean" model.

macOS never quiesces the iBoot-started DCP in order to boot a proper one. It
**adopts the running generation**:

* iBoot *injects* `pre-loaded = 1` and `running = 1` onto `iop-dcp-nub`. Neither
  property exists in the shipping `DeviceTree.j700ap-25G83.bin`. `quiesced` is
  in the shipping ADT and is **not** removed — all three coexist on a live boot
  (verified with `ioreg -p IODeviceTree -n iop-dcp-nub -l` on J700 / 25G83).
* `RTBuddy::_initConfigEDT()` reads them **by presence**, not value.
* `AppleDCPExpert::start()` then diverges at three `iBootLoaded()` call sites:
  start in `DCPPowerState 4` rather than 0, disable the display Sys/Fe power
  gate, configure the PMGR scratch region in iBoot-loaded mode, and read and
  apply `remote-power-state` from the provider.
* `no-firmware-service` means no firmware blob is fetched at all.

Live traces agree: a captured macOS boot opens with a single `A401`
(`start_signal`) and contains no RTKit HELLO/EPMAP or firmware-load traffic
whatsoever. It attaches to a coprocessor that is already up.

### The STARTEP gate constrains macOS, not Linux

`RTBuddyManagementEndpoint::setEndpointStatus` builds the STARTEP word and then:

```text
ldr x8,[x0,#0x70] ; ldr w8,[x8,#0x158] ; cmp w8,#4 ; b.lt -> return 0
```

It returns without sending STARTEP, and logs nothing, whenever the IOP state is
below 4. **Scope this carefully:** that is macOS kext code, and it explains why
*macOS* cannot be made to cold-boot a quiesced generation. Linux does not use
RTBuddy — `apple-drm` has its own RTKit implementation and sends STARTEP itself,
so this gate does not apply to it. Do not cite it as evidence that Linux cannot
restart a stopped ASC.

### What this implies for m1n1

Adoption is Apple's path, and it is the one known to work on this hardware:
hand Linux a **live** DCP, publish `pre-loaded` and `running` on the DCP nub, and
carry `remote-power-state`, so the driver attaches at IOP state >= 4 without a
firmware load and without gating the display fabric. `apple-drm` already has the
adoption mode for this (`adopt_running_coprocessor`, set only for T8140), which
engages when no loader-handoff property is present and the coprocessor is still
active.

That is the higher-confidence route, but it is **not proven to be required**.
The driver also implements a restart from the stopped state, and that path has
never been exercised, because the node is disabled before it can be. Un-disable
the node first and find out; only redesign the handoff if the restart genuinely
fails.

Note that `quiesced` being present is *not* what blocks adoption on real
hardware — macOS boots fine with it set.

## 5. m1n1's own DCP access is a different protocol

m1n1 drives DCP over the **iBoot** protocol (`src/dcp_iboot.c`, opcodes like
`IBOOT_SWAP_BEGIN`), not IOMFB's A4xx RPC. Nothing in the IOMFB method table or
the swap record layout applies to m1n1, and fixes on one side do not propagate
to the other. Do not "align" them.

## 6. Swap ABI, from live traces

Established from macOS 25G83 traces captured under the m1n1 hypervisor
(20 boots; 1,579 swaps in the longest), and fixed in this branch:

| | value | note |
|---|---|---|
| `swap_start` | **A406**, `0x10` in / `0x08` out | moved from A407 on v13_3 |
| `swap_submit` | **A408**, `0xe9c` in / `0x0c` out | did **not** move; stayed at A408 |
| completion | `D590`, `0x730` in | 1:1 with each swap |
| modeset | `A441`, `0x84` in / `0x7c` out | |

A407 is never called on H17P — not once, in any traced boot. The H17P swap
record carries **no `surf2` block**, and `dcp_swap_h17p` is `0x58a`
(`h17p_tail` is `0x122`, not `0x278`), giving `dcp_swap_submit_req_h17p` =
`0xe9c` exactly.

## 7. Status

Everything in §6 is derived from captured traces and static RE and verified by
compiling the struct definitions; **none of it has been exercised on hardware**,
because of §3. Treat the first boot after §3 is resolved as bring-up.

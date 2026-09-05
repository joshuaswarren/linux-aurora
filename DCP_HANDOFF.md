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
changing it is a contract change, not a bug fix. The argument for changing it:
steps 3 and 4 above already guarantee no inherited state reaches Linux, which is
exactly the "fresh start" the code claims — the `status = "disabled"` adds
nothing beyond locking the driver out, and `apple,dcp-asc-stopped` already tells
the driver what it is inheriting. There is currently no build flag or `/chosen`
property to opt out; the disable is unconditional once state 2 is reached.

Until that changes, DCP driver work on J700 can only be validated by tracing
macOS under the m1n1 hypervisor (see `MACOS_HV_AGX.md` in the m1n1 tree —
DCP tracing rides along with `--agx-full`).

## 4. m1n1's own DCP access is a different protocol

m1n1 drives DCP over the **iBoot** protocol (`src/dcp_iboot.c`, opcodes like
`IBOOT_SWAP_BEGIN`), not IOMFB's A4xx RPC. Nothing in the IOMFB method table or
the swap record layout applies to m1n1, and fixes on one side do not propagate
to the other. Do not "align" them.

## 5. Swap ABI, from live traces

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

## 6. Status

Everything in §5 is derived from captured traces and static RE and verified by
compiling the struct definitions; **none of it has been exercised on hardware**,
because of §3. Treat the first boot after §3 is resolved as bring-up.

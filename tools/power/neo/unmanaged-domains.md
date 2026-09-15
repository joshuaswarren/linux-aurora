# Unmanaged J700 domains: experiments, not a shutdown policy

Claude's 2026-09-15 inventory broadened the search beyond Linux's live genpd
providers. The retained ADT contains 296 device entries, including virtual
entries. `j700-pmgr-domains.json` is Claude's decoded table; original input is
workspace `artifacts/neo-suspend/dcp-pmgr-native/adt-pmgr.json`, SHA256
`211b4c39963829532b1e9c60e47027059d039b7c338205043c152b5caf3f4193`.
An ON register read taken awake is not proof of the state throughout sleep,
and absence of a Linux consumer is not proof that firmware never accesses it.

Claude's first PRORES+MSR gate reached actual 0 and lowered the first SMC PSTR
reading after a 20s sleep from about 0.7702 to 0.7535 W. This is a promising
screening result, not a calibrated sleep average. The original restore passed
`f` to base-0 strtoul, silently setting target 0 instead of 15. Both blocks
therefore stayed off; restoration was not qualified. A subsequent external
display trial gated GPPP/DBE, then stopped at a DISPEXT0_CPU write and required
DebugUSB recovery. Do not repeat CPU-domain writes from this tool.

The imported tools have now been tightened:

- C numeric parsing rejects partial/overflow/unaligned arguments before MMIO.
- Restore requests explicit `0xf`, verifies ACTIVE, restores original AUTO
  policy if needed, and checks policy readback. Failed writes/reads abort.
- Restoration records retain readbacks and boot ID; cross-boot replay is
  refused. Restore failures keep unresolved state and stop before children.
- CPU domains and busy/reset/device-disable words are rejected. Powered
  children, including descendants behind virtual entries, prevent gating;
  there is no force bypass.
- The outer trial exits unsuccessfully on failed sleep, failed restore,
  storage/network recovery or increased PM failure counters. An IPv4 address
  alone no longer counts as network recovery; association and gateway response
  are required. No hardware test is implied by these offline checks.
- PM snapshots now record Type-C roles/partners and battery/AC state. The
  unexplained +1.2 W in earlier awake AND sleep measurements makes changed
  physical/boot state a lead; it does not prove which load was responsible.

The SMC series immediately after wake is useful for fast screening. Values
can be stale and are not an integrated sleep measurement. `sleep-smc.py` now
ends its battery-counter interval before the following four seconds of awake
SMC sampling and rejects external-power/non-discharge samples. Use a fresh
awake calibration and 120s battery-counter ABBA to confirm any proposed saving.
SMC rail names/overlaps alone do not establish an exhaustive power budget.

Offline regressions:

```
python3 tools/power/neo/test-pmgr-gate.py
python3 tools/power/neo/test-validate-result.py
```

`test-pmgr-gate.py` reproduces OFF readback being accepted as successful
restoration and checks failure propagation. The C helper was also executed
under qemu-aarch64 with `f`, target 16, an unaligned offset and an unknown
command: all rejected before opening /dev/mem.

Do not reuse the original `domain-gate/chain.sh`: its grep-based acceptance
is inadequate. The revised trial lives in a fresh `domain-gate-v2/` remote
stage. Kernel #40 itself remains unchanged; nvme_phy_s2idle stays disabled.

Initial tooling and inventory: Claude Fable 5.1; restoration and validation
corrections: Codex. Original scripts/feedback were retained under workspace
`artifacts/neo-suspend/domain-gate-review/` before modification.

# Guest35 N1 startup tracing

Guest35 used carrier32 SHA256
`71d486a98638b238e1680200bd0d2da2748db19cd1f5a959cff4550289b098cd`
and `M5_RETAIN_RESIDENT_BOOT_POLICY=1`. System cryptex authentication and
SEP/userspace startup passed again. Resident Stage1 was unchanged.

## Hardware observations

- Native enumeration: 106b:1900, BAR0 size 0x10000, RID 0x0100 / SID 1.
- Passive prearm attached physical BAR0 0xb82000000 before MEM enable,
  reading PCI configuration only. Native PC 0x19fb34760 then read offsets
  8004=00900681, 8008=20260840, 800c=2602e402, 8010=2b494916,
  and 8000=0 twice. Journal:
  `logs/n1-traces/n1-1788970884583856000.jsonl`.
- A subsequent guarded live context check verified current identity and
  MEM enable, but rejected IPC status 8050=0 before reading DMA payloads.
- AppleCentauriControl PID225 started; centaurid PID276 followed. At
  kernel time 342.2736, PCIe disableGated began; L2 timed out, the link went
  down and the control device was removed. No native MMIO write or TX/DMA
  submission was captured.
- The optional CentauriControlSystemEvents child reported Start failure.
  Exact driver code can intentionally stop this child when the
  `system-events-required` property does not request notifications. That
  log alone does not establish the shutdown cause.
- A nearby BootPolicyUserClient start assertion concerns provider cast /
  superclass start; it is not proof of a signed-policy digest failure.

The latest ring is `logs/msgbuf/xnu_msgbuf_20260909_123404_04.raw` with its
JSON bufx/size receipt. Decode the entire rotated ring; the general .txt
decoder can omit the tail after wrap. The final pause was ordinary EL1
MSR at 0xfffffe000b76c398, not a panic. SEP had 645 mailbox records.

## Bounded process inspection

`read-process-state.py` followed the exact stopped-kernel PID hash and
found centaurid PID276 at proc 0xfffffe1e3efe14e0. PID225 was absent. Exact
task/map/pmap accessors led to root physical 0x1032e4e0000, with verified
native 16-KiB page geometry. `read-user-memory.py` reads existing resident
pages only; absent/compressed mappings are rejected and no page is faulted
in, remapped or written.

`logs/guest-35/centaurid-resident.macho` reconstructs 22 resident pages and
records 17 missing pages as holes in `centaurid-resident.json`. UUID is
8D0DD22B-0D13-3B45-B936-B17662C6715B; live load address 0x102614000,
ASLR slide 0x2614000. It contains loader relocations and runtime data and
is not a complete original executable. Ghidra project:
`j813-centaurid-live-g35` / `centaurid-resident.macho`.

Resident Objective-C method tables identify activate: at 0x10000a154,
state at 0x100012908, firmware-present getter at 0x100012924, builtIn at
0x100012930, hasHardware: at 0x100012380 and run at 0x1000016e4. Startup
function 0x1000229f0 has separate activation-failure and missing-firmware
power-off paths and schedules a 600-second deferred retry after failed
activation. This source observation does not identify which path ran.

The chip constructor caches CentauriBooterIsFirmwarePresent at object
offset 0x31, hasHardware at 0x30 and builtIn at 0x32; chip state is at
0x38. The powerOff method's native call to CentauriControllerSetChipPower
returns at image offset 0xc178 with the chip object retained in x19.
That is a candidate user breakpoint after the kernel shutdown entry.

Guest36 will arm AppleCentauriManager::powerOnOffSequence at
0xfffffe0008fe162c before startup, then inspect the caller and daemon state.
Guest35's breakpoint was armed only after removal and did not hit during
40 further guest seconds. Its controller was terminated before guest36.

Linux DHCP remains unresolved. No known-good native TX descriptor or
packet-delivery comparison has been captured yet.

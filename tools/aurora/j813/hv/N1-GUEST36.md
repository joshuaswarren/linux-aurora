# Guest36: N1 powers down because firmware is unavailable

Same carrier32, policy and headless configuration as guest35. Native N1
enumerated 106b:1900, BAR0 size 0x10000, RID 0x0100 / SID1. Passive ASYNC
prearm was attached before driver startup.

The first pause verified the exact 16 bytes at kernel
AppleCentauriManager::powerOnOffSequence, 0xfffffe0008fe162c, then armed a
hardware breakpoint. It caught arguments false, false, true before port
shutdown, from a native user-client call. Kernel ring:
`logs/msgbuf/xnu_msgbuf_20260909_131144_02.{raw,json}`.

The running caller was traced through centaurid PID269's existing native
16-KiB pmap. Its UUID matched the guest35 image; all 16 bytes at each
candidate user breakpoint were checked before arming. The kernel breakpoint
was removed. The next stop was EL0 PC 0x102d18178, image offset 0xc178,
returning from CentauriControllerSetChipPower(false) in the powerOff method.
Current-context translation matched centaurid's independent page walk.

Live evidence in `logs/guest-36/centaurid-poweroff.json` (mode 0600):

- Return code 0: native power-off request succeeded.
- Chip object 0x926c88000: hasHardware=1, firmwarePresent=0, builtIn=1.
- State was 1 before the powerOff method's subsequent store of state 5.
- Caller return address was image offset 0x23344. Exact startup code at
  0x23340 calls powerOff specifically on the missing-firmware branch.
- Next caller was image offset 0x1b74, the daemon's startup dispatch block.

This establishes the shutdown reason, not merely a timing correlation.
The firmware-presence predicate checks personalized firmware in its default
location, then an unsigned-firmware/global-manifest fallback. Live native
code identifies the personalized root helper as ACFUDataAccess, with a
sharedDataAccessor path request. Its full failure result is not captured.

## Boot handoff mismatch and next comparison

Current guest boot-uuid and apfs-preboot-uuid select
AAE717A6-D9CB-4900-8695-FC62B8DD4C26, the full macOS installation. However,
boot-objects-path still names stub volume group
154559C3-3E4F-42C3-B06B-16A9F72CEA78 and its 8649B037... boot hash. The
verified native IODeviceTree capture instead names the full macOS group
and 10E0792B... boot hash. This is a confirmed handoff mismatch; its causal
role in firmware discovery still requires the comparison.

`M5_NATIVE_BOOT_OBJECTS_PATH=1` changes only that guest filesystem path.
The path is bound to the captured native volume UUID and signed nsih.
`M5_RETAIN_RESIDENT_BOOT_POLICY=1` continues retaining the resident LP,
iBoot ticket, LP measurement and boot-manifest-hash that passed SEP.
Host checks cover both captures, serialization, default path preservation,
unchanged SEP pair and rejection of an unbound path before mutation.

Guest36's controller was terminated after preserving the evidence.
Guest37 uses the unchanged carrier32 with this one additional flag.
No installed file, resident Stage1, signature check or root check changed.
Native N1 TX capture and Linux DHCP remain unresolved.

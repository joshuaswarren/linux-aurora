# M5 macOS hypervisor trace preparation

## Current status: native trace identified the Linux DHCP fix

Guest37's native DHCP descriptor exposed missing packet-preparation value 4
in Linux. A same-association 0/4/0 crossover isolated that field, and the
default-enabled fix now passes DHCP and bidirectional traffic on a fresh
native Linux RAM boot using the normal TX queue. See
[Linux packet evidence](../N1-DATA-RINGS.md) and the
[working setup](../README.md).

Guest38 booted with time stealing disabled, received a native DHCP ACK,
and passed three router pings with zero loss. SSH verified Mac17,3/25G76.
A temporary caffeinate assertion prevents the known idle-sleep failure.
This supplied the working baseline for Linux's packet-metadata comparison.
See [N1-GUEST38.md](N1-GUEST38.md).

## Guest37 captured a native DHCP transmit

`M5_NATIVE_BOOT_OBJECTS_PATH=1` passed the guest36 missing-firmware failure
on hardware. The unchanged carrier32 and matched resident LP/iBoot pair
now boot N1 firmware, complete both control and alpha IPC handshakes, and
open the Wi-Fi datapath. A WSYNC observer captured native TR43 descriptors
and payloads, including a 342-byte DHCP Discover. Native footer 0x20000,
zero expiry and queue flags 8 differ from Linux's current diagnostic TX
settings. Clearing accumulated stolen time recovered native networking
briefly, verified by SSH. A later idle-sleep SEP DART fault halted guest37;
guest38 kept time stealing disabled and inhibited idle sleep, completing
the native lease baseline. Linux then qualified the preparation-field fix.
See [N1-GUEST37.md](N1-GUEST37.md).

Runtime PCI identity must be refreshed after ROM boot: function 0 becomes
control 106b:1901 at 0xb82020000, function 1 is Wi-Fi alpha 106b:1902 at
0xb82010000, and function 2 is beta 106b:1903 at the reused ROM BAR
0xb82000000. Later accesses at the original prearmed address belong to
beta; do not report them as Wi-Fi TX.

## Guest35 captured native N1 register reads

Guest35 hardware-qualified passive prearm on the assigned control BAR at
0xb82000000: six native reads, including registers 8004..8010. Control IPC
status remained zero. No native writes or TX/DMA submission was observed
before the control device powered down. centaurid remained alive after
the control driver exited. A bounded reconstruction of its resident code
identified distinct shutdown reasons, which need a live call-stack capture
to distinguish. See [N1-GUEST35.md](N1-GUEST35.md). Guest35's controller was
stopped after read-only captures; guest36 uses unchanged carrier32.

## Guest34 passed SEP and reached userspace

Guest34's `M5_RETAIN_RESIDENT_BOOT_POLICY=1` comparison passed the former
SEP `boop/boop` panic. It authenticated os.dmg, app.dmg and os.clone.dmg,
launched fsck, airportd, wifiFirmwareLoader and centaurid, and continued
to userspace. SEP logged 681 messages; the last pause was ordinary MSR
`0xfffffe000b76c398`, with the kernel log reaching 397 seconds versus
root mount at 318 seconds. No signature or root check was patched.

N1's control driver was subsequently closed when centaurid startup
coincided with PCIe disableGated at 342.224 seconds. The port timed out
waiting for L2, the link went down, and IOKit removed 106b:1900. No later
re-enumeration appeared. This is the next capture target, not evidence
that Linux TX/DHCP is fixed. Guest34's controller was stopped after the
read-only log captures; guest35 will use the same carrier32 and policy.

`attach_enumerated(..., prearm=True)` now permits passive ASYNC watching
of a native-assigned BAR before MEM enable. It reads PCI configuration
only. Active context reads revalidate live identity, MEM enable and BAR
assignment first. Host tests cover passive-only prearm and rejection of
active reads against disabled/missing devices. Guest35 qualified the passive
trap and captured native register reads.

At an EL0 pause the kernel mapping can be absent. `read-msgbuf.py` now
allows a physical read only for a static buffer previously translated
and verified by this same controller, with unchanged header VA, guest
base, buffer pointer and size. Stale, dynamic, moved and EL1-unmapped
cases are rejected by host regression tests. Raw ring bytes and a
provenance receipt accompany each capture.

## Guest33: mixed SEP policy handoff

Guest33 reproduced guest32's SEP `boop/boop` firmware panic with a passive
126-message mailbox trace. Read-only inspection of the retained shared
buffer proves its native LocalPolicy nsih does not match SHA384 of the
resident iBoot manifest sent alongside it. The original resident policy
does match that iBoot manifest. This is a confirmed handoff mismatch;
the exact SEP assertion is not yet decoded. See [SEP-GUEST33.md](SEP-GUEST33.md).

Guest34 used the same carrier32 with the opt-in
`M5_RETAIN_RESIDENT_BOOT_POLICY=1` comparison. It retains the measured
resident LP/iBoot pair and boot identity while keeping the validated
native cryptex query mirrors. Host tests pass the signed nsih/ticket
regression and reject corrupt input before modifying the guest ADT.
Resident Stage1 is unchanged. Linux DHCP remains unresolved.

## Guest32: page recycling passed, then SEP firmware panicked

Carrier32 passed guest31's page-recycling failure. Guest32 authenticated the
cryptexes, ran fsck/fsck_apfs, and continued through SEP/xART exchanges.
Its first four-second pause was an ordinary MSR trap. After eight more
seconds it was at panic HVC `0xfffffe000b7bbdd0`; do not resume it.
Native panic format: `SEP Panic: %s\n%s`. The private report identifies
`boop/boop`, app 289.160.2, UUID B921B0DF-5242-3F0F-90C5-931C85322B4B,
SEPOS 3151.161.2, and stack
`0x100026a60 0x589e4 0x589c8 0x10001ea78 0x1000204d4 0x100020580 0x10002052c 0x1000009a0`
with token `[hgggimqmg]`. The exact rejected condition is not decoded yet.
Evidence: `logs/guest-32/sep-panic-{message,info}.bin`,
`logs/msgbuf/xnu_msgbuf_20260909_112143_02.{txt,raw,json}` and
`logs/guest-32/console.log`.

N1 control was enumerated as 106b:1900, BAR0 size 0x10000, RID0x0100/SID1.
At the first pause memory decoding was still disabled, so the guarded
attachment stopped. The N1 trace remains empty. The guest32 controller
was stopped after read-only panic captures; no Linux DHCP fix exists. A
scoped VDM reboot again returned directly to m1n1. Guest33 is uploading
with the same carrier32 binary and the passive SEP mailbox journal.

SEP handoff comparison found the same `sepi` firmware measurement as native
macOS, but inherited `effective-production-status-sep` and
`effective-security-mode-sep` are both 0; native macOS reports both 1.
The resident ADT also reports 0, so these are not guest-only overrides.
`enable-sep-rm=0` and `sepfw-load-at-boot=1` agree after type normalization.
Do not assume the mode differences or missing `rsepfirmware` diagnostic
identify the cause. A private resident SEPFW capture is being inspected.

The resident SEPFW capture completed (5,947,392 bytes); its wrapper contains
an IMG4 at offset 24. `ipsw img4 extract` produced the original `sepi`
IM4P and signed manifest. The extracted payload has no Mach-O header or
`boop` string and does not provide a decoded app reference. Do not treat
those opaque payload bytes as executable disassembly.

`M5_SEP_TRACE=1` is prepared for guest33. It enables `trace-sep-m5.py`, a
passive ASYNC mailbox journal guarded to the measured M5 SEP register
geometry. It logs raw fields, tags and RX-discovered endpoint names to a
private JSONL, capped at 8,192 records. It does not instantiate a DART,
read shared payloads, or issue device writes. Host tests verify field
decoding, RX-only discovery, the limit and passive mode. No SEP-reported
security mode or signed policy is changed for this diagnostic run.

## Guest31 page-reference accounting correction

Guest31 passed the internal-display fault using the headless configuration.
It authenticated Macintosh HD and successfully grafted `os.dmg`, `app.dmg`
and `os.clone.dmg`, with successful root-hash verification for each. This
hardware-validates the native policy/ticket-selection handoff. It reached
launchd starting `/sbin/fsck`, then panicked in `pmap_recycle_page` at
`pmap_data.c:2334`: physical page `0x103b9844000` was still referenced.
Its FTE was `00000b00000000000000000001000000`: type 11, RO count 0,
writable/executable count 1. N1 tracing is still empty. Evidence:
`logs/msgbuf/xnu_msgbuf_20260909_110403_01.{txt,raw,json}` and
`logs/guest-31/{console.log,sptm-prefix.bin,pmap-*-disasm.txt}`.

Source review found that `update_refcounts_for_pte_change_geom` ignored
permission changes when the physical address stayed the same. Native
25G76 reads separate RO and writable/executable counts at FTE+8/+12.
A writable-to-RO update followed by unmap could therefore leave precisely
the observed stale writable reference. The production-C host test reproduces
the failure before the fix. Carrier32 now transfers the reference between
these counts when permissions change. The test passes both page sizes,
RW/RO/RX/software-writable transitions, independent aliases, replacement,
unmap, secondary/table frames and invalid entries. Three existing frame-kind
checks also pass via direct invocation; pytest is absent from the venv.
The hardware failure's exact lifecycle still needs the next boot test.

Carrier32 builds successfully. Private artifacts are `artifacts/carrier-32/`:
bin SHA256 `71d486a98638b238e1680200bd0d2da2748db19cd1f5a959cff4550289b098cd`,
ELF SHA256 `870732cd794cff77848122bdc28aee246e1120640be3f355ad83432f948bf69d`.
The guest31 controller was stopped before rebuilding. A scoped M5 VDM
reboot then returned directly to normal m1n1 USB, without native macOS or
physical startup-disk verification. Carrier32's RAM chainload was accepted
and guest32 is running the next test. Do not assume every reboot needs user
interaction: recheck normal USB and SSH first. Do not reconnect an older
guest using the newly built ELF's symbol offsets. Resident Stage1 is
unchanged. Guest32 uses both display-isolation flags and the validated
native-macos-after29 policy capture.

## Guest30 internal-display fault

Guest30 used the fresh policy capture below, and the staged native `lp-*`
fields read back with no `lp-smb0`/`lp-smb1`. It stopped before root mount,
launchd, N1 enumeration or cryptex authentication, so the ticket-selection
correction remains unqualified on hardware. Its native panic is
`dart-disp0`: SID 0 PTE-invalid read at DVA `0x100025b5000`,
`AppleT8110DART.cpp:2369`. The guest remains paused at panic HVC
`0xfffffe000b7bbdcc`; do not resume it. Evidence:
`logs/guest-30/console.log` and
`logs/msgbuf/xnu_msgbuf_20260909_104817_01.{txt,raw,json}`.

Read-only inspection found TCR `1`, TTBR `0x103ffedd`, L1 entry
`0x103ffee401`, and leaf `0xfff103e24c003`, matching the panic's reported
TTE. Cached DART state has two calls and zero carrier violations. The
precise cause of the display fault remains unresolved.

For guest31, `M5_ISOLATE_INTERNAL_DISPLAY=1` with external-display isolation
removes the ten measured internal display/DCP nodes from the guest ADT.
It preserves N1 PCIe/DART and shared audio nodes byte for byte through an
ADT serialization check. This is a headless tracing experiment, not a
display-driver fix; hardware validation is pending. The carrier22 binary
and resident Stage1 are unchanged.

Guest30 also exposed an incorrect bound in the prepared N1 context reader:
BootArgs `phys_base` is the loader's usable range (`0x10003084000`), not
DRAM base. The reader now uses native `chosen.dram_base=0x10000000000`
and `mem_size_actual=0x400000000`, validates the usable range is contained,
and continues rejecting DMA translations outside DRAM. Actual geometry and
out-of-bounds rejection checks pass. No N1 DMA capture exists yet.

## Guest29 ticket-selection diagnosis and guest30 policy experiment

Guest30 tested a correction for the ticket-selection cause. Exact native 25G76
AppleImage4::_darwin_el2_get_secure_boot (`0xfffffe00095927b4`) adds
`lp-smb0` and `lp-smb1`: m1n1's inherited fields sum to2. Exact native
dyld::_cryptex_open (`0x85304`, selection near`0x85890`) deliberately
chooses the reduced/generic manifest at levels1/2. Native macOS has
neither flag (level0). `native-macos-policy.py` now validates every native
`lp-*` field against the signed LocalPolicy, replaces the guest's stub
`lp-*` mirrors with those values, and publishes the corresponding native
boot-manifest hash. The guest's expected hash and ticket-selection level
therefore come from the same captured policy. No authentication code is
patched. Hardware validation is pending.

The current capture is `logs/native-macos-after29/`; its IODeviceTree
SHA256 is`2bfbc1790e06f45e8cb29c9a7b041067849b7115e570f0cc78739042cd4a9a0b`,
LocalPolicy IM4M SHA256 is
`02d86d6cb68108b40a437c3db67ba6c4707c4cea74e79959e0ad68ffe0a76160`,
and signed generation is9. Startup-disk verification regenerated the
policy/tickets during the native return, so guest30 uses this fresh set.
Both old and current capture bindings pass host validation. Native
userland references were copied while the M5 remained at its desktop:
`kernel/userland-25G76/dyld.arm64e`, `launchd`, and extracted libraries.
The separately labeled host26A5425a references are not the M5 source.

The native return after guest28 and guest29 required startup-disk
verification on the M5's physical screen. SSH and Screen Sharing were
unavailable there; the user completed both prompts. Do not assume a hard
reboot can return to native macOS without that interaction.

Guest29 now reads the captured `lp-spih` correctly. The failure changed
from missing property (ENOENT) to a hash constraint mismatch. The actual
manifest hash in the native message buffer exactly equals SHA384 of
`apticket.j813ap.im4m` (the generic ticket), while the chip-side hash equals
the signed policy and `apticket.j813ap.104C83C09401C.im4m` (personalized).
This rules out a stale policy as the explanation for this mismatch.
Investigate native ticket selection rather than changing the expected
hash or patching authentication. Evidence:
`logs/msgbuf/xnu_msgbuf_20260909_101940_01.{txt,raw,json}`.

The guest was paused four seconds after entry, at the ordinary MSR trap
`0xfffffe000b76c398`, before its later SEP timeout. N1 control had been
enumerated as RID0x0100/SID1 but memory decoding was still disabled, so
the read-only BAR attachment correctly stopped without probing BAR RAM.
Guest29 was stopped and the M5 restarted for native reference inspection.

## Guest28 to guest29 handoff correction

Guest28 passed the corrected PAPT copyout path and reached launchd/N1
enumeration, but publishing the measured LocalPolicy IM4M alone did not
resolve cryptex authentication. Exact 25G76 AppleImage4 disassembly at
`0xfffffe0009593440` shows that property 0x19 reads
`/chosen/asmb/lp-spih`, which was absent from the inherited m1n1 ADT.
Native macOS supplies this 48-byte value. It matches both signed `spih`
and SHA384 of the personalized cryptex ticket. Guest29 stages that field
and the native signed `stng` generation before the ADT snapshot/seed.
The source bindings and serialized ADT roundtrip pass host tests.

Guest28's later panic was `SEPEpoch::getFullEpochs`, `SEPEpoch.cpp:73`,
after AppleSEP timed out waiting for endpoint reply `lo: 0x170600` at
368.862 seconds. External-display isolation avoided the earlier
DISPEXT0_CPU panic in this run. NVMe completed 6,808 calls with zero
violations; MTE tagged 3,568 pages with zero rejections. Evidence:
`logs/guest-28/console.log` and
`logs/msgbuf/xnu_msgbuf_20260909_095740_01.{txt,raw,json}`.

The ECAM trace remained empty despite native PCI enumeration. Exact
AppleEmbeddedPCIEPort::configRead32 at `0xfffffe0009222a90` confirms a
PIODMA path that can bypass CPU ECAM traps. `N1PCIJournal.attach_enumerated`
now supports guarded, read-only attachment to a function already proven
present by native enumeration. It requires the native BAR size and checks
identity twice, memory decoding, alignment and ADT range translation.
Host tests reject absent/disabled/malformed targets and provide no write
method. This fallback still needs hardware validation; no DMA payloads
have been captured.

`read-n1-context.py` is prepared for a paused guest whose N1 BAR has been
attached and whose current RID-to-SID association is known. It accepts
only completed IPC handshakes and validates the context header, ring
counts and DRAM bounds before saving the context/index/message-ring
bytes. It uses the existing DART tables with a fresh Python cache and
never initializes the DART or sends invalidation/mapping writes. Syntax
and RAM-boundary rejection checks pass; hardware use remains pending.

## Native policy capture used by guest28 and guest29

After guest27, the M5 booted its native25G76 desktop. SSH confirmed
`Mac17,3`, `25G76` and useradmin at`Admins-MacBook-Air.local` using
existing key access. Protected capture directory:
`logs/native-macos-20260909/`. Its IODeviceTree SHA256 is
`10618f65ad78a1bba62ce114bf08dad85d1fcf9a07ef7606481d85371b892896`.

The captured active LocalPolicy's IM4M is2,846 bytes, SHA256
`a22c1cdda2d9a3d814db0f39da8e054c6d9659a64e3ed3a7fda58998af9cfb8e`.
Its SHA384 equals native `/chosen/secure-boot-hashes/lpol`. Its signed
`spih` equals the SHA384 of the personalized cryptex ticket, while the
generic ticket does not match. Its chip, ECID, volume group,25G76 version,
`nsih` and policy filename/`lpnh` also match the native boot capture.
This supplies a measured source for the missing cryptex hash.

`M5_NATIVE_POLICY_CAPTURE=<capture directory>` validates those bindings,
stages the original signed IM4M bytes, then seed.py allocates a guest RAM
page, verifies readback and updates the LocalPolicy manifest pointer,
corresponding measured hash and the validated asmb cryptex fields. Other boot-policy metadata is retained;
this is a bounded manifest-handoff experiment, not proof of a complete
native handoff. Host tests pass source bindings, ADT serialization and
cross-chip/device/profile/volume rejection. Guest cryptex authentication
still requires hardware validation.

Guest28 also enables `M5_ISOLATE_EXTERNAL_DISPLAY=1` (the exact dispext0/1
and dcpext0/1 guest node families) and `M5_N1_TRACE=1` before guest entry.
Camera isolation and native `clpc=0` remain; broad PMGR tracing stays off.
`macos-next-m1n1.py` uses the already installed m1n1 volume with native
`bless --nextonly`, followed by a clean M5 restart. It does not install an
image or change the default startup disk. Carrier22 remains unchanged.

## Guest27: copyout passed; later cryptex and external-display issues

Guest27's new bootstrap layout passed the guest26 copyout failure. XNU
validated the on-disk root hash, started launchd, mounted Preboot, loaded
a type13 trust cache and published AppleCentauriManager. NVMe completed
6,796 calls with zero violations; MTE tagged3,824 pages in63 calls with
zero rejects. These are hardware results for the lower PAPT correction.

Two later issues are separate. AppleImage4 reports that its cryptex1
manifest hash is absent (ENOENT), so the system cryptex graft fails.
Exact25G76 AppleImage4's activation routine at0xfffffe000958ee20 obtains
that hash from the `spih` property of an IMG4 manifest. The inherited
m1n1 proxy handoff and full Macintosh HD selectors are not yet a complete
same-volume boot handoff. Investigate the full installation's signed
LocalPolicy/manifest metadata; no authenticity check has been bypassed.

After continuation, guest27 panicked at AppleEventLogHandler.cpp:248 on a
non-deferred write to locked PMGR register0x380700428. The live ADT names
this `DISPEXT0_CPU` (device113, parent111). Its stack runs through
ApplePMGR::_enableDeviceGated. This external-display power failure is
independent of the cleared copyout fault and the cryptex hash omission.
Guest27's controller was terminated after saving the panic evidence.

Evidence: `logs/guest-27/console.log`,
`logs/msgbuf/xnu_msgbuf_20260909_092546_01.{txt,raw,json}`,
`logs/msgbuf/xnu_msgbuf_20260909_092918_02.{txt,raw,json}`, and
`logs/pmp-traces/irq-m5-1788960375616677000.bin`.
Decode wrapped rings with retained indices. N1 tracing was attached only
after Centauri publication; `logs/n1-traces/n1-1788960417831558000.jsonl`
is empty. `M5_N1_TRACE=1` now supports tracing from the next bootstrap.
No native N1 TX/DMA capture or Linux DHCP proof has been obtained yet.

## Guest26: launchd reached; copyout zone-metadata fault

Guest26 used native `clpc=0`, exact camera isolation and selective PMP
tracing, with unchanged carrier22. PMP status became1 and XNU logged
`PMP started`. It authenticated and mounted Macintosh HD's read-only
snapshot XID433 on disk4s1, then started `/sbin/launchd` (PID1). NVMe
completed4,040 calls with zero emulator violations; MTE tagged3,696 pages
in61 calls with zero rejects. The resident IRQ journal recorded2,305
events. This proves PMP readiness, authenticated root and early userspace.
Native N1 TX capture, graphical login and Linux DHCP remain pending.

The guest stopped at a kernel data-abort panic and its controller was
terminated after diagnostics; guest27 will test the corrected layout.
PC0xfffffe000b86d780 is the metadata load in `zone_element_bounds_check`,
called by `copyout` while copying physical file-buffer data to userspace.
Source0xfffffe13a5cdc000 translates to PA0x103bd8b0000. Its expected zone
metadata0xfffffe4c86e47370 is unmapped (ESR0x96000007). Our lower M5 PAPT
starts0xfffffe1000000000 and overlaps XNU's observed zone range
0xfffffe1018000000..0xfffffe3618000000. Investigate the bootstrap virtual
layout and compact-pointer contract; do not bypass the bounds check.

Guest27 moves the M5 PAPT to0xfffffe0100000000, above the kernelcache and
below XNU's heap start0xfffffe1000000000. With guest26's RAM geometry,
the PAPT ends0xfffffe04c4a2c000 and its early arena spans
0xfffffe04c6000000..0xfffffe04f0800000. The old copyout buffer would
be0xfffffe04a5cdc000, outside the zone range. Production layout tests
reproduce guest09's pointer truncation and guest26's zone overlap, then
check corrected addresses, RAM-size variants, invalid layouts and the
unchanged Neo geometry. The earlier label identifying +64 GiB as
VM_MIN_KERNEL_ADDRESS was incorrect: exact25G76 reconstructs pointers
from -2 TiB, while +64 GiB begins the general kernel heap.
Hardware validation of the correction is pending.

Evidence: `logs/guest-26/console.log`,
`logs/pmp-traces/m5-1788959221942846000.jsonl`,
`logs/pmp-traces/irq-m5-1788959317860457000.bin`, and
`logs/msgbuf/xnu_msgbuf_20260909_090806_01.{txt,raw,json}`.
The JSON now retains ring indices and raw SHA256 for wrapped-log recovery.

## Guest25: PMP exchange traced; CLPC sampler interrupted progress

Guest24 lost KIS transport while uploading kernel chunk0x4400000; no guest
entry occurred. The M5 loader now stops on any unverified chunk instead of
treating a recovered NOP as proof of its bytes. SIGQUIT stack diagnostics
are installed before upload as well as during guest execution.

Guest25 completed upload and seed with the exact camera family removed and
selective ASYNC PMP MMIO tracing. Its resident journal proved385 PMP IRQ1003
acknowledgements delivered to guest CPU6. RTKit hello, endpoint discovery,
buffer setup and IOReporting exchanges occurred. The observed ApplePTD
read at0xfffffe0009873954 confirms the status address0x3803c0010; its reads
were still zero. Thus RUN and interrupt delivery are proven, readiness is
not. The generic Neo DAPF initializer must not be copied: M5 dart-pmp's
additional register apertures are RWSP, not FPAD/FPAG.

Guest25 first paused on CorePerfSampler::enable selector writes at
0xfffffe0009bb6ad0 and0xfffffe0009bb6ad8 in AppleT8142CLPC. Both
EL2 writes raised UNDEFINED. An optional coherent per-CPU guest shadow
(`M5_CLPC_SAMPLER_SHADOW=1`, default off) passed production-handler tests
and allowed diagnostic continuation. Counter reads then generated frequent
host round trips and distorted timing. CLPC power samples were not valid.
The controller was paused, evidence saved, then stopped for a fresh boot.
Guest26 will use Asahi's standard `clpc=0` boot control without this shadow;
its behavior on25G76 still needs hardware confirmation. Carrier22 is unchanged.

Guest25 retained2,435 tagged pages in41 calls with zero rejects. NVMe had
only its initial call, so this run did not reach the earlier storage milestone.
PMP DART had128 calls (66 maps,58 unmaps), zero violations. Evidence:
`logs/guest-25/console.log`,
`logs/pmp-traces/m5-1788958345394476000.jsonl`,
`logs/pmp-traces/irq-m5-1788958918121095000.bin`,
`logs/dart-state/dart-6-1788958387847775000.bin`, and
`logs/msgbuf/xnu_msgbuf_20260909_085307_01.txt`.
The later085951 raw ring is wrapped; its TXT contains only the post-wrap
prefix, so inspect the retained raw bytes for complete chronological content.

Reference: [Asahi m1n1 hypervisor guide](https://asahilinux.org/docs/sw/m1n1-hypervisor/).

## Guest23: PMP readiness timeout

Guest23 selected the full Macintosh HD volume group and reached148,712
SPTM calls,3,187 tagged pages,1,704 NVMe calls and10,535 DART calls, with
zero MTE rejects or NVMe/DART emulator violations. It panicked in
ApplePMGR::_waitForPMPReadyActionGatedv2 because die0 PMP did not report
online within30 seconds. A read-only paused snapshot found ASC CPU_CONTROL
0x10 and CPU_STATUS0x6d: its RUN bit is set, but that does not establish
PMP readiness. Root authentication and userspace remain unproven.

USB0 DART completed its power-up and13 map calls without the guest21 replica
assertion. This is hardware evidence for the disabled-register replay fix,
not a full USB qualification. The ISP DART error storm again overwrote much
of the kernel log. Guest24 uses exact camera-node removal (a substring
`isp` would also remove display nodes), disables broad synchronous PMGR
tracing, and journals only PMP control/mailbox/PTD accesses asynchronously.
The new tracer performs no DMA reads or extra MMIO accesses. Its PTD status
address is explicitly a candidate pending native ApplePTD verification.

Evidence: `logs/guest-23/console.log`,
`logs/msgbuf/xnu_msgbuf_20260909_082941_01.txt`,
`logs/dart-state/dart-10-1788956981704419000.bin`, and
`logs/pmgr-traces/m5-1788956883587715000.jsonl`.
Guest23's controller was stopped before scoped M5 reboot. Carrier22 remains
unchanged; the next run uses the same verified kernel and root inventory.

## Guest22: mounted the inherited stub root volume

Guest22 reached135,528 SPTM calls,2,931 tagged pages and1,182 NVMe calls,
with zero MTE rejects and zero NVMe/DART emulator violations. XNU returned
success from mounting`disk2s2`, then panicked at the root-authentication ioctl
(`rootvp not authenticated after mounting`, bsd_init.c:979). The guest
inherited boot UUID`154559C3-3E4F-42C3-B06B-16A9F72CEA78` from m1n1.
The new read-only inventory confirms this is the volume-group UUID of
`m1n1 proxy`, a System volume containing18 files, no snapshots, and no APFS
integrity metadata. `Macintosh HD` is the full installation:272,091 files,
three snapshots, integrity metadata OID71484, volume group
`AAE717A6-D9CB-4900-8695-FC62B8DD4C26`. Its individual System-volume UUID is
`002B5AD3-DE49-4769-8CD7-2F3273416FBC`; do not confuse the two identifiers.

`read-volume-inventory.py` read430 GPT/APFS metadata blocks through the
resident normal USB m1n1 proxy, with GPT CRCs and APFS object checksums.
No disk writes were issued. The private receipt is
`logs/volume-inventory/m5-1788956350201548000.json`.
`M5_MACOS_VOLUME_INVENTORY=<receipt>` now selects the full macOS volume group
for both guest root selectors. It checks M5 identity, the current stub
selectors and an unambiguous full System volume. The production helper
passes receipt, unchanged-handoff and rejection checks. Other iBoot handoff
properties retain their bytes; correct root authentication is still pending.

This is evidence of real storage/root-mount progress, but not successful
root authentication or userspace. Also, USB0 DART has zero calls in this run,
so its replica correction remains unqualified on hardware. The camera
`dart-isp` generated a large error-log burst before the root panic; it did not
cause the observed final panic. The msgbuf provenance check passed, but the
128-KiB static ring already overwrote earlier boot messages.

Evidence: `logs/guest-22/console.log`,
`logs/msgbuf/xnu_msgbuf_20260909_080701_01.txt`,
`logs/dart-state/dart-10-1788955621444443000.bin`, and
`logs/pmgr-traces/m5-1788955501349938000.jsonl`. Guest22 stopped in panic;
its controller was stopped and the target rebooted to the normal USB proxy
for the inventory. Guest23 will test explicit root selection. Neo volume
IDs are unrelated.

Guest21 passed the energy-restore fault and reached134,846 SPTM calls,
2,931 tagged pages in49 calls, zero MTE rejects and1,182 NVMe calls with
zero violations. It then panicked in AppleT8110DART::_recoverFromPowerDown,
line3284, for`dart-usb0`. The assertion compares each non-exclave stream's
TTBR across all DART instances after the SPTM power-up call`0x8117`.

Read-only hardware inspection found exactly one replica mismatch: SID14 at
`0x402f00000` had TCR1/TTBR`0x3fec96bc`; the second instance at`0x402f80000`
had TCR0/TTBR`0x247f5f74`. Both TTBR values are invalid. Cached dart-id10
SID14 was valid state but disabled (TCR0, no root, stream disabled), with
TTBR`0x247f5f74`. `dart_c_finalize_replay` omitted that register image because
it had neither a root nor translation/remap/bypass enabled. This left the
two hardware copies inconsistent.

The C replay now includes known disabled SID register images, still excluding
exclave-owned and unknown state. It does not enable the stream or translation.
`test-dart-replay.py` runs the production function against the captured values:
the original implementation reproduces the failed replica comparison, while
the correction passes disabled-state, live-root, raw-replay, exclave, unknown
SID and repeated-replay checks. All19 existing DART source checks also pass
(run directly because this environment does not have pytest). Carrier22
built successfully and is uploading guest22 after a verified USB proxy
handoff. Its bin SHA256 is
`889ddb8581d1c5357776be7e3c03f888642712190fd1c65f8a15d4b54db64ef0`;
raw ELF SHA256 is
`c25f9b37542fb972b1a8ed093fc96618529d8506c6efe32074a370c05974f98d`.
Copies are retained in`artifacts/carrier-22`. The previous controller was
stopped before rebuilding the carrier and its ELF symbols.
No root mount or native N1 trace has been obtained yet.

Evidence: `logs/guest-21/console.log`,
`logs/pmgr-traces/m5-1788954433968613000.jsonl`,
`logs/msgbuf/xnu_msgbuf_20260909_074850_01.txt`, and the private cached state
`logs/dart-state/dart-10-1788954865003498000.bin`. `read-dart-state.py` derives
the relevant structure fields from the matching C source and reads RAM only.

Guest20 reached the pre-write breakpoint at journal event3353, with a healthy
KIS controller. `M5_ACTIVE_KIS_VERIFIED` confirms the new pre-entry transport
path worked. The captured stack is writeReg64 ->
`_restoreCPUEnergyAccumulators` (`0xfffffe000983dca0`) -> restoreACC ->
AppleT8142PMGR::restoreACC -> restoreHW -> AppleT8142PMGR::restoreHW -> start.
The exact target setter is setCPMEnergyAccumulator, which forwards the
cluster energy value to offset`0xe48000`. Its M5 counter-enable method is
empty. Removing generic perf-counter metadata would not avoid this
unconditional energy-restore call.

The optional `M5_SKIP_CPU_ENERGY_RESTORE=1` diagnostic now omits just the
call at`0xfffffe000983cce4`, leaving inherited accumulator values and the
rest of power restoration intact. Default is0. The full kernel hash and a
13-instruction signature guard it. `test-cpu-energy.py` passes one-opcode
comparison, surrounding-code rejection, chip isolation and controls; it
also reruns the CQ test. Guest21 passed that access and progressed through
PMGR restoration; energy-accounting correctness is not qualified.

Guest20 measured116,583 SPTM calls,2,295 tagged pages in39 tag calls, zero
MTE rejects, and1,180 NVMe calls with zero violations. The earlier CQ panic
has not repeated, but root mount is not yet demonstrated. The private msgbuf
dump at`logs/msgbuf/xnu_msgbuf_20260909_072830_01.txt` has its header and every
static-buffer page verified against the current uploaded image. It shows
PMGR initialization and scheduling delays, without a root-mount marker.
Evidence is in`logs/guest-20/console.log` and
`logs/pmgr-traces/m5-1788953240788364000.jsonl`. The pending write was not
forwarded. Userspace, native N1 capture and Linux DHCP remain unfinished.

Guest19 finished preparation but did not enter XNU: the pre-entry DebugUSB
helper timed out. Host diagnosis found `/etc/pam.d/sudo_local` enables
`pam_reattach` and `pam_tid`, so sudo can wait for Touch ID before consuming
the supplied stdin password. This explains the intermittent authorization
timeouts; they must not all be attributed to HPM transport failure.
`host_auth.py` now supplies the authorized Keychain credential to macOS
authorization through osascript stdin. No secret is in argv or logs and no
PAM/Touch ID setting was changed. Scoped nop and reboot both completed in
under a second. M5 guest entry now verifies chip, iodev and NOP on its existing
KIS controller and retains that working link instead of rearming it again.

Guest18 applied the CQ correction and reached a different failure before
storage qualification could be collected. Its PMGR journal recorded a
64-bit zero write to `0x211e48000` from exact ApplePMGR `writeReg64` at
`0xfffffe0009841998`; the proxy then took SError at its completion barrier
(`proxy.c` dsb sy, carrier relative PC `0x2fd8c`). Repeated exceptions left
the proxy unavailable. The controller was terminated and the target recovered
through separate scoped reboot/debugusb calls. No fault was resumed or hidden.

The address lies in M5 PMGR reg18 (`0x211e40000`, size`0xc020`). Earlier
64-bit reads in that range succeeded; this was the first observed write to
the failing address. Guest20 used `M5_PMGR_BREAK_ACC=1` to enter the tracer's
retrying debug shell before forwarding that store. The callback records the
registers in its private journal. Do not skip/continue that pending write;
inspect the stack, cached NVMe state and guest msgbuf first. This breakpoint
does not provide a register result or alter the pending guest instruction.

`read-nvme-state.py` reads cached resident RAM using the C prefix layout.
It reproduces guest17's queue state from the retained snapshot. `read-msgbuf.py`
adds M5 chip/hash checks and verifies the header and static buffer against
the currently uploaded kernel before saving private logs. A dynamic buffer
is explicitly marked as requiring separate provenance review.

Guest17 removed the guest-only MTP family and passed the earlier DART panic.
It reached 111,266 SPTM calls, 2,179 tagged pages and zero MTE rejects, then
ANS2 panicked: `CQ (Host I/O) DB error`, head62, err_info_1=`0x20040`.
Read-only resident inspection found 162 NVMe calls, zero violations,
queue_count=max_cid=64, and AQA=IOQA=`0x003f003f`. The live CAP register is
`0x20f0010040`, whose zero-based MQES encodes 65 entries. This matches the
Neo CQ wrap mismatch documented in the requested branch review.

The exact M5 IONVMeFamily `ProcessCompletionQueue` begins at
`0xfffffe000a9ecba8`; its wrap count load is `0xfffffe000a9ecc14`.
The RAM guest loader now replaces that one `ldr w10,[x22,#0x10]` with
`mov w10,#64`, guarded by the complete kernel hash and nine instruction
signatures. `M5_NVME_CQ_DEPTH_64=0` disables this control. All other Neo
device-specific patches remain excluded. `test-nvme-cq.py` passes exact
single-opcode comparison, rejection of altered surrounding code, chip
isolation and disabled/invalid controls. Hardware qualification is pending.

For guest17 recovery, combined VDM reboot/debugusb timed out and an overlapping
carrier load failed. The target subsequently enumerated as normal USB CDC;
separate scoped debugusb followed by carrier17b succeeded. Wait for each
operation to finish before the next. `host_auth.py` now also accepts a separate
scoped `reboot`; fresh-process reboot, wait, and debugusb avoid reusing a
controller handle across the target's reconnect.

Guest16 reached 109,861 SPTM calls and 2,179 tagged pages in 37 calls with
zero rejects. XNU panicked in AppleT8110DART.cpp:2369: `dart-mtp`, SID 0 STE
invalid on read of DVA `0x3881f008c`, summary `0x2000`, AXI ID `0x38`.
That guest was left paused until its subsequent recovery; no successful userspace boot exists.
The PMGR journal has 4,442 events, overwhelmingly MCC counter reads. Its MCC
overlap is now writes-only to reduce overhead. This different first failure
does not qualify guest15's asynchronous SError as fixed.

Guest17 removed the guest-only MTP node family (`--strip-node mtp`):
dockchannel-mtp, mtp, dart-mtp and mtp-aop-mux. The saved topology's MTP mapper
references are confined to that family and PCIe remains present. This is a
headless trace-guest isolation experiment, not a DART implementation fix.
See `BRANCH-TRACE-REVIEW.md` for the requested live GitHub branch review and
the DART-aware packet capture work still required for N1.

Guest15 passed the static-object and `threads_ro` panics with ordinary
kernel mappings and no tag-entry stub. Resident counters reached78,557
SPTM calls and1,027 tagged pages in19 calls with zero rejects. It reached
device initialization, then stopped on asynchronous SError (ESR0xbe000000)
while PC=`0xfffffe0009c50590` was in the exact `AppleT8142PMGR` image.
An asynchronous exception does not identify the offending access by PC.

The host diagnostic then tried to clear the locked L2C error register and
exited. `ProxyUtils.print_l2c_regs` now skips these registers when the CPU
feature reports them locked; a production-method test verifies no accesses
in that case. `inspect-resident.py` recovered counters without resuming or
modifying the stopped guest. Guest16 booted with `M5_PMGR_TRACE=1`.
`trace-pmgr.py` journals guest-requested PMGR accesses, retaining higher
priority reserved UART/CPU hooks. No SError has been ignored or resumed.

Guest14 independently passed the timer stall and reached37,997 SPTM calls,
13 tag calls and643 dynamically tagged pages with zero rejects. Its new
panic is `threads_ro` retagging: an ordinary read-only element resolved to
tag0 and was incorrectly treated as allocation-tagged. A bounded hardware
probe toggled only TCR.MTX0/MTX1 around one LDG: the same pointer changed
from `0xf0fffe15dc02d540` to canonical `0xfffffe15dc02d540`. The TCR,
debugger state, PTE and16 data bytes were verified unchanged afterward.

The M5 seed now sets TCR.MTX0/MTX1 alongside TBI1, matching XNU's canonical
tag behavior. The earlier static-tag stub, altered kernel-page attributes
and extra storage references were removed. Ordinary kernel and PAPT mappings
are retained, and XNU starts at its original entry. Tests for the production
MTE seed and timer helpers pass. Guest15 passed the simpler canonical-tag
fix with unchanged carrier14 C. `probe-canonical-mte.py` preserves the live
diagnostic. macOS userspace, native N1 capture and Linux DHCP remain pending.

## September 9 continuation: static kernel tags

Guest10 was interrupted during upload. Guest11 freshly rebooted via scoped
RID1, loaded the unchanged carrier08, verified the complete kernel upload,
and exercised the lower M5 PAPT layout. It reached a different VM panic:
`addr ... being freed to zone vm objects, isn't from zone map` at zalloc.c:1150.
Its SPTM ring has36,376 calls; dynamic MTE counters remain3 successful tag
calls and zero rejects. The existing controller was paused before inspection.

The static VM-object pointer is `0xf0fffe000c71e500`. Exact kernel instruction
`0xfffffe000b8b62b8` reconstructs its tag with LDG before deallocation.
The kernel mapping's PTE was ordinary memory, and an independently stepped
LDG returned tag0. `probe-static-mte.py` tested one granule: tagged mapping
alone still returned tag0; architectural STG with the static default tag15
made LDG return `0xfffffe000c71e500`. The original tag, PTE, debugger state,
and all16 data bytes were verified restored. No TagOffset or direct
tag-storage RAM write was performed. The already-panicked guest was not resumed.

The earlier candidate mapped both writable kernel segments and their PAPT
aliases as tagged normal memory and executed an88-byte tag-only stub over565
pages. Guest13 passed the static-object panic with this workaround. Guest14
later identified missing canonical-tag controls as the underlying issue;
the stub and its dedicated test have been retired in favor of that fix.
`inspect-kernel.py` provides read-only disassembly of the hash-pinned kernel.

The user supplied host and target credentials and asked to stop password
prompts. They are stored in the login Keychain, never in this repository.
`host_auth.py` retrieves the host item and supplies it through osascript stdin
to password-authenticated macOS authorization for scoped RID1 operations.
Host authentication is verified; this avoids this Mac's sudo Touch ID wait.
`run-guest.py` uses that helper; the prior administrator dialog was removed.
The target account is `admin`; its Keychain service is
`aurora-silicon.m5-macos`. The host service is `aurora-silicon.host-admin`.
The continuation has no runtime webhook variable, so no Discord URL was
searched for or substituted. The reporting contract below remains applicable.

Guest12 verified the88-byte tag-entry stub but stopped before guest entry:
the single15,814,944-byte frame-table readback timed out after15,764,696 bytes.
`../kis-transport.py` now reads in1-MiB transactions and hashes every byte.
Tests cover successful multi-chunk readback, corrupt data, short reads,
decompression-length rejection and timeout restoration. The1024-byte writes,
gzip padding/original decompression length and full SHA256 check remain.
Guest13 completed all readbacks and reached37,789 SPTM calls,11 tag calls,
515 dynamically tagged pages and zero rejects. Two paused snapshots showed
XNU looping in FIQ handling with CNTV_CTL=7 and CVAL=0. The carrier was
discarding the guest's IMASK bit when deciding whether to inject a virtual
FIQ. Disabling that one source in the existing paused controller let XNU
advance to38,007 SPTM calls and643 tagged pages before a different panic.

The new fault is an ST2G at `0xfffffe000b99db88`, called by zone retagging
at `0xfffffe000b869528`. ESR=`0x9600004f`, FAR=`0xf7fffe187dfaab00`;
its leaf PTE=`0x4600103bc51c683` has AP=2 and AttrIdx=0, and the physical
frame is type25. This permission/mapping problem remains unresolved.
Some page tables live in the tag-storage range, but the observed examples
cover unmanaged physical memory and may legitimately be reclaimed by XNU.
Their location alone is not evidence of tag-storage corruption.

Carrier14 separates guest architectural timer masks from host-imposed masks.
`test-arch-timer.py` compiles the production helpers and tests the measured
failure, real pending deadlines, rearming, per-CPU isolation and non-M5
semantics. The ARM build, RAM chainload and guest14 passed the timer change.
Artifacts are in `artifacts/carrier-14/`. `read-fault-state.py`
decodes the verified panic format through the existing paused controller.

The user authorized macOS hypervisor tracing on September9 to compare a
successful native N1 connection and DHCP exchange with the Linux driver.
This supersedes the earlier preference to exhaust Linux-only experiments.

The target is J813/T8142/Mac17,3, dedicated KIS
`/dev/cu.kis-01100000-ch-0` and scoped host HPM RID1. Preserve resident Stage1
and internal disks. Never use the Neo RID0 port or its unscoped VDM service.
Guest08 executes XNU and3 memory-tag operations under the hypervisor.
A macOS userspace boot and an N1
hypervisor trace have not been produced on the M5 yet.

`m1n1/` is an isolated detached worktree of `m1n1-aurora` at
`a2a1db46e9f3d2c1a8fb96fe23d484e73f27edae`, the proven Neo SPTM baseline.
It does not modify the Neo or the working J813 Linux carrier. Existing
J813 Windows/UEFI hypervisor code lacks this baseline's SPTM implementation.
Do not transplant Neo PMGR/ACC addresses or kernel patch offsets into M5.

## Exact kernel

Local IPSW `OS Images/apple/UniversalMac_26.6.1_25G76_Restore.ipsw` has three
J813 identities, all selecting chip0x8142,board0x24 and
`kernelcache.release.mac17g`. `ipsw extract --kernel --device Mac17,3`
produced the decompressed Mach-O at
`kernel/25G76__Mac17,3/kernelcache.release.Mac17,2_3_4`,120995840bytes,SHA256
`43263b4f4b7cd29cb0cc487ba944b5bf0a8f3b24b8b1cad875b796d9af026189`.
This matches the hardware's previously SSH-verified OS build26.6.1/25G76.

Commpage sites verified by the actual offline loader (not hardware-qualified):

| Field | M5 address | Expected opcode |
| --- | --- | --- |
| USER_TIMEBASE | 0xfffffe000b983720 | 0x52800068 |
| CONT_HWCLOCK | 0xfffffe000b983778 | 0x52800028 |
| HW_TPRO | 0xfffffe000b983974 | 0x39043128 |

Each instruction/store pattern has one aligned match in M5 __TEXT_EXEC;
the surrounding instructions agree with the same-build Neo commpage code.
The loader enforces the full-kernel hash and all three instruction signatures.
It patches SPTM genter entry, guarded sysreg writes, these three commpage
fields, and the M5-qualified NVMe CQ site described above. Other Neo-specific
follow-up patches are skipped. Prepared-image SHA256 with the CQ correction is
`45a5793eafdd1a60e9ae550b38ae10add131622fe3e2e08c4fb710aa1ee702aa`;
the disabled control remains
`20e411be1c009a9221d4556fafd7b9f6f9afcd20f67cc7fc0adf780e63244cbc`.
Both have entry offset85032960. The actual full loader was run offline for
both controls and differs at exactly that one instruction. A wrong kernel
is rejected before upload.

## Carrier qualification

The explicit `T8142_KIS_PROXY=1 CHAINLOADING=1` build preserves the inherited
KIS proxy and bypasses device initialization after memory/AIC/watchdog setup.
It performs no PMGR cleanup, DCP/PCIe/SEP initialization or secondary CPU
release during carrier startup. Carrier02 is hardware-proven: repeated
chip/transport checks and three NOP handshakes pass. Carrier01 timed out
before a proxy; comparing the proven M5 carrier identified that Neo's
default extra64-KiB DockChannel offset must be omitted on M5 too. Carrier02
changed only that routing and reached its ready marker. No PMGR, DCP, PCIe
or SEP initialization ran. Early MMU/AIC/watchdog/SAPT setup did run.

Carrier02 artifacts are preserved in `artifacts/carrier-02/`:

| File | SHA256 |
| --- | --- |
| m1n1.bin | 31bef92a6f22aaa407482bb99a58bfe2221213e67e5fe435ff4a75e00123eadf |
| m1n1.elf | 231d6aa7e2ee5079d03e87350604fa13a462da15900ea9aba2ba9579165dbb95 |
| m1n1-raw.elf | d83d555eec4c1ed85b49376252f983f3fdb6faeb972195c6ebc010141e550230 |

Carrier03 enables SPTM init on explicit T8142 qualification builds and has
reached the proxy on hardware. Guest03 verified the entire kernel upload and
initialized SPTM on physical boot CPU6, then aborted before entry on
`missing T8110 DART DAPF base: dart-aop`. The protected pre-seed ADT snapshot
contains28 DARTs; several have DAPF descriptions with no FPAD/FPAG aperture.
RWSP entries are power registers and must not be used as DAPF slice bases.
The M5 seed now preserves inherited physical filter state, while retaining
all171 derived page-table/boot-premap ranges. Offline execution of the actual
inventory builder passes28 DARTs and zero physical DAPF replays. This is a
qualification setting, not proof that every device can power-cycle safely.

Guest04 uploaded the kernel and successfully seeded28 DARTs and171 ranges
on hardware, but a later Python check rejected the intentionally empty
physical-DAPF inventory. The M5 exception now extends through that check and
skips the C replay-seed call entirely. Offline execution of the complete
Python DART configuration/range/replay block passes against both saved ADTs,
with exactly the two expected configuration and range calls. A fresh reboot
is required after either partial seed; neither attempt entered macOS.

Carrier03 artifacts are preserved in `artifacts/carrier-03/`:

| File | SHA256 |
| --- | --- |
| m1n1.bin | d3c2fc7f43a1f8c3cbb214334fd3d76fefcee4571d2da457e8f096d51db2a138 |
| m1n1.elf | 6cb872d283e8e028be1ea6311d0fe52b5ccc737fffcfb23e8369d5a5a206f9cf |
| m1n1-raw.elf | 06a362b2c14baf5be3aaa7c51bcc07263a7a660b94dc9f0a25217a96b1533151 |

SMP initialization records the current boot CPU and returns before any
RVBAR or CPU-release access. Guest boot requires `cpus=1`, retaining the
complete CPU table for indexed SPTM allocations; a requested secondary start
is trapped and rejected. M5 ACTLR remains untouched and virtual UART updates
do not clear physical UART registers. Later guest06 proves XNU entry, as
described below.

`run-guest.py` fixes the target endpoint and adapts DebugUSB requests to the
RID1 tool. Its administrator credential stays in process memory and goes
to osascript only over stdin. Required bootstrap failures abort guest launch.
`bootstrap-m5.py` calls the generic SPTM seed without Neo radio/PMGR hooks.

`n1-trace.py` is a prepared, hardware-untested passive PCIe journal. It
observes bus1/device0 config cycles, validates Apple N1 product identity and
observed BAR size, then traces the first64KiB of BAR0 only inside an ADT PCIe
memory window. It never probes or writes hardware. Offline event replay
passes32/64-bit windows, mask decoding, address rejection and device removal.
The private journal is0600 and stops at100000 events. It records register
ordering only; DMA command/packet snapshots still need implementation.
Guest04 remains an uninstrumented boot qualification, so this tracer is not
automatically enabled by the bootstrap script.

Use the established1024-byte KIS transport, compressed padding/original
length, full SHA256 readback and RAM bounds. Reboot through
`../reboot-m5.py` before a new chainload. Never install this carrier.
After the user's manual restart, the KIS endpoint required a scoped
`../reboot-m5.py --debug-usb-only` re-arm. Chainload05a-c did not reach the
resident proxy;05d verified the carrier and reached its ready marker.
Guest05 used the corrected complete DAPF preservation path, with no N1
tracer. Its saved console is `logs/guest-05/console.log`.

Guest05 completed the entire SPTM bootstrap and reached the guest-launch
path. An EL2 synchronous exception then rebooted the target: relative
PC0x163f8 in carrier03's raw ELF is `hv_update_fiq` at `src/hv_exc.c:851`,
opcode `msr s3_5_c15_c1_3,x0` (Apple VM_TMR_FIQ_ENA_EL2). J813's existing
carrier already documents this register as write-locked on T8142. Carrier06
adapts its architectural timer-mask helpers and keeps a host-masked expired
timer pending. Actual extracted C helpers pass behavioral assertions for
M5 mask/pending behavior and unchanged earlier-SoC gate behavior. Incremental
cross-build passes with69 existing warning emissions and zero errors.
macOS userspace and N1 initialization are still unproven; the EL2 failure
does not establish a successful macOS boot.

Carrier06 is preserved in `artifacts/carrier-06/`:

| File | SHA256 |
| --- | --- |
| m1n1.bin | f771357e4067b1e5ff6ea35486118029076b4a8686e14cce292a16fd78be37d9 |
| m1n1.elf | 7b5e1ee390a185b6a19844edfd84d784a36a00402cbb4a3249192f6b6ba70706 |
| m1n1-raw.elf | 7bbdf0cfc36bee69414c83bb64143e4255811714a0322c4d344d5d24b5abf81b |

After the guest05 target panic/reboot, the M5 DebugUSB-only re-arm succeeded.
The new carrier must be loaded before retrying the guest; never rebuild its
raw ELF while a guest controller is using it for SPTM symbol resolution.

Guest06 validates the timer fix on hardware: XNU executes and calls the
SPTM console hypercall. Its first panic is `Mismatch of ARM_LARGE_MEMORY in
SPTM (%#llx)/XNU (%#llx)`. The guest was paused through its own controller
for read-only inspection. The copied SPTM block at0xfffffe0007d87408 agrees
with the supplied block except for XNU's consumed/cleared entropy bytes.
No live guest memory was changed.

Exact same-build kernel comparison identifies a larger M5 boot-data ABI:
M5 copies0x350 bytes at0xfffffe000b9655c8, versus Neo's0x340-byte seed.
M5's capability check at0xfffffe000b965af0 loads offset0x348 and requires
bit4; Neo's corresponding check at0xfffffe000b58adc4 loads0x338. M5's
variant-pointer consumer0xfffffe000b975ebc uses0x318, and its IO range/filter
consumers0xfffffe000b962918..0xfffffe000b96293c use0x328/330/338/340.
The kernel upper bound is consumed at0xfffffe000b967a08 from0x2f8.
The libsptm v10 initializer still consumes the same populated fields through
0xd8 relative to the embedded library block at0x1a0, and passed on guest06.
The M5 seed now shifts every populated tail field by0x10, preserving the
Neo layout. Production packing expressions pass offline assertions for both
layouts. This corrects the ABI instead of bypassing XNU's capability check.
Guest07 passed this capability check on hardware with the corrected layout.

Guest06 was left paused, its controller was terminated, and a scoped M5
reboot completed. Carrier07 reloads unchanged carrier06 C artifacts;
guest07 tests only the Python boot-data ABI correction. Its console is
`logs/guest-07/console.log`. The N1 tracer remains disabled for boot isolation.
`read-sptm-console.py` can be loaded through a paused controller's
`hv.run_script` to decode the last64 SPTM calls from resident RAM. Its
2736-byte structure and ring offsets168/2728 match the compiled C source
and carrier06 symbol size. The normal SK print budget is48 calls, so this
read-only decoder can recover a later panic suffix that was not printed.

Guest07 subsequently trapped on `msr s3_5_c15_c1_6,x8` at
0xfffffe000c0aeb10, writing0x1c000000 after a read of zero. The production
trace-control register rejects the forwarded EL2 write. The Python handler
now shadows reads and writes per CPU only for a verified J813 kernel. An
offline test exercised the actual method without a hardware proxy. The
method was then loaded into the paused controller and handled the trapped
instruction, advancing ELR by4; the same guest resumed and progressed.
The shadow preserves guest register state without enabling hardware tracing.

Guest07 stopped at an unhandled domain0/table0 SPTM selector35 (0x23),
return PC0xfffffe000c0e6d18. Its native wrapper starts0xfffffe000c0e6d00,
with callers0xfffffe000b954b9c and0xfffffe000b954ca8. Arguments are a physical
page-list pointer, count1, and zero. Native table entries at
0xfffffff02701a560/570 prove selectors35/36 are
`sptm_tag_papt_multipage` and `sptm_untag_papt_multipage`. The implementation
and exact tag-storage handoff evidence are recorded in `MTE-NOTES.md`.
Guest07 was terminated while paused before rebuilding, then the target was
rebooted through scoped RID1. Carrier08 passed the first3 tagging calls on
guest08. A later tagged-address fault was proven to need TCR.TBI1 by a
reversible translation check on the paused guest. Guest09 passed that fault
and reached36,380 SPTM calls, then failed decoding a compact VM-map pointer
because the Neo PAPT/adjacent arena base exceeded its representable range.
Guest10 reloads unchanged carrier08 C and tests a lower M5-only virtual
layout. The M5 controller has exclusive KIS ownership; guest10 console is
`logs/guest-10/console.log`.
Do not open a second proxy or rebuild a live carrier's matching ELF.

Before guest10, the M5 reboot enumerated as standard m1n1 USB (VID0x1209,
PID0x316d at host location0x01100000); its stale KIS node timed out. No
process owned the endpoint. Re-arming DebugUSB alone did not restore it.
A read-only normal-USB probe verified chip0x8142/iodev4. The carrier loader
now accepts `--j813-source-usb <callout> --vector-only` only after matching
that exact M5 USB topology and validating the proxy identity. All RAM span,
readback, firmware preservation and RVBAR guards remain. This path loaded
unchanged carrier08, released normal USB, then a scoped DebugUSB-only re-arm
restored the verified KIS chip0x8142/iodev1 connection. Guest10 uses KIS.
Logs: `logs/chainload-carrier-10-usb-b.log` and
`logs/carrier-10-kis-identity.log`. Re-discover the current callout from
IORegistry each time; do not assume a remembered USB serial path.

## Linux checkpoint retained

The last RAM Linux44 checkpoint was authenticated with normal TR25 and
`station_link_addresses=1`; no44 packet probe has run. The pending native
unicast probe was built, but not uploaded or executed when the user selected
hypervisor tracing. The hardware-proven directed receive result remains42b.
Broadcast43 DHCP/ARP tests failed; their complete protected archive is saved.
The44 station/address archive is `re/n1-identity-44.tgz`,326B,SHA256
`ce1cefbecdbe4dc03b29fe5b97fb25d7f6cfbf7bc92832700d39fed8e98d09aa`.

Milestones use the existing runtime-only webhook contract at
`/Users/mcruz/.codex/attachments/877c20f2-0018-42bb-9097-021833f19a67/pasted-text.txt`.
Reread it after context loss. Do not store the endpoint, credentials, network
names or identifying capture contents in this worktree's documentation.

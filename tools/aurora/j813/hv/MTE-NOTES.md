# M5 memory-tag bootstrap and endpoint evidence

Status: guest15 passed both allocator panics with TCR.MTX0/MTX1 enabled and
the static-tag workaround removed. It reached1,027 tagged pages with zero
rejected requests, then asynchronous SError in device initialization.
Guest16 will trace PMGR accesses. Historical evidence
follows below. The user-authorized purpose remains
native N1 tracing for the Linux TX/DHCP failure.

Exact native inputs are the 25G76 M5 kernel and
`OtherResources/IPSW-T8142-25G76/Firmware/sptm.t8142.release.bin`.
The latter is imported read-only in Ghidra project `j813-sptm-25g76`.

Native table0 begins at0xfffffff02701a330, with16-byte entries. Entry35 at
0xfffffff02701a560 points to0xfffffff0270fdb90; entry36 at0xfffffff02701a570
points to0xfffffff0270fdbf0. The first accepts only option0x100 (deferred
TLBI), supplies fourth argument1, and branches to0xfffffff0270fd4ac.
The second supplies zero options and fourth argument0. Strings identify
tag/untag PAPT multipage operations. The common implementation:

- Copies1..64 physical page addresses from guest RAM and validates each
  managed, aligned frame, XNU ownership/tag permission, unmapped state, and
  current tag flag. Data-frame mapping counts are32-bit fields at8/12.
- Resolves a tag-storage frame by
  `tag_base + (((page_pa - dram_base) >> 5) & ~0x3fff)`.
  That frame must have type32, with its reference count at byte4.
- Sets the data frame's byte4 bit2 and increments the storage count when
  tagging; clears/decrements after invalidation when untagging.
- Changes only PAPT AttrIndx to4 (tagged) or0 (untagged), and invalidates
  translations. Deferred tagging returns5; ordinary completion returns0.

The native PAPT updater is0xfffffff0270dd540. The emulator conservatively
completes invalidation even for deferred requests and preserves return5.
It preflights whole batches, rejects duplicates and invalid state without
mutating RAM, and stops at unsupported frames instead of faking success.
This is qualified only for the enforced single-CPU M5 guest.

Native bootstrap0xfffffff0270bcf84 derives tag storage from read-only
`s3_0_c11_c9_0` (TagOffset), mask0x3fffff00000, and `/chosen/dram-size >> 19`
pages. It checks the register's lock bit63. Guest07 read
0x80000103bc600000, DRAM base0x10000000000 and size0x400000000. The32,768
tag-storage pages occupy0x103bc600000..0x103dc600000, exactly the top512MiB
of the inherited RAM range. The prior seed incorrectly left these frames
as XNU_DEFAULT. No tag-storage memory or TagOffset register was written.

Native XNU handoff begins0xfffffff0271042c0. Bootstrap0xfffffff0270bf9f0
writes tag count at0xfffffff027104598 (argument offset0x2d8) and tag base at
0xfffffff0271045a0 (offset0x2e0). These are the16 added bytes before the
previously corrected M5 tail. The embedded libsptm block at offset0x1a0
receives tag base/end at offsets0x70/78 and flags bit0 at0xa8. The seed now
populates those fields, marks all tag frames type32, and rejects bootstrap
allocations crossing into that region. Native taggable frame-type data
prefixes are copied without native function pointers. MAIR index4 becomes
0xf0 for tagged normal memory; earlier SoC seeds remain unchanged.
The M5 bootstrap then enables HCR.ATA and SCTLR.ATA/ATA0 after verifying
architectural MTE2. Tag-check fault controls are left to guest policy.
This uses the existing proxy HCR setter and changes no physical TagOffset.

`test-mte.py` extracts and compiles the production C functions against RAM
mocks. It verifies tag/untag, shared tag-storage reference counts, duplicate
and malformed batch rejection without partial writes, mapping-count guards,
option/length bounds, deferred return codes, and the M5 seed guard. Tests
passed. Separate execution of the production seed AST verified inherited
geometry, unlocked/mismatched-range rejection, both argument blocks, and
all32,768 reservations. ARM cross-build passed, with88 warning emissions;
the new handler's2128-byte stack exceeds the conservative2048 warning
threshold by80 bytes. No compile errors.

Carrier08 artifacts are preserved under `artifacts/carrier-08/`:

| File | SHA256 |
| --- | --- |
| m1n1.bin | 522c48d47f3dd211e1c8ed35a38d6ac8f5f7e153dd99c39372fe4077e6231031 |
| m1n1.elf | 9bef6d96588b51453625229b0c85107f0cde36251e989439008281aa51a24046 |
| m1n1-raw.elf | 5169ed6046736f6c458343a200dfeee284d94e24c7b783702a7573f45a39c613 |

Initial tagging operations are hardware-proven. Untagging, tag-check fault
policy, tag-storage reclaim, macOS userspace, N1 tracing, and successful
Linux transmission/DHCP remain unproven.

Guest08 hardware counters report3 successful tag calls,0 untag calls,
3 tagged pages and0 rejected requests. The guest later halted after a store
at0xfffffe000b90dd14 faulted on0xf0fffe000c67b884 (ESR0x96000044). Its
untagged alias0xfffffe000c67b884 translates to0x1001d80b884. A reversible
experiment while paused set TCR_EL12.TBI1 (bit38): the tagged address then
translated to the identical PA. The original TCR was restored immediately;
the panicked guest was not resumed. The M5-only seed now sets TBI1 for the
next boot. Earlier SoCs keep the prior TCR value.

Guest09 passed the tagged-address failure and reached36,380 SPTM calls
(versus8,898 on guest08), still with3 tag calls and0 rejected tag requests.
It then faulted in compact VM-map pointer decoding at0xfffffe000b8d0964.
Saved registers prove packed value0x0f1d274b reconstructs
0xfffffe03c749d2c0, while the original mapped object is at
0xfffffe43c749d2c0 (PA0x100219e12c0). Shifting the original pointer right6
and truncating to32 bits exactly reproduces the stored value: bit38 is
lost. This is a bootstrap virtual-address layout error, not a tag mismatch.

The M5 seed now starts PAPT at0xfffffe1000000000 (the exact kernel's observed
VM_MIN_KERNEL_ADDRESS), with its adjacent bootstrap arena checked against
the31-bit/64-byte compact-pointer ceiling0xfffffe2000000000. The previous
Neo base0xfffffe4000000000 remains unchanged for other SoCs. Guest10 will
test this M5 layout; it is not yet hardware-qualified.

## September 9 continuation: guests11-14

Guest11 exercised the lower PAPT layout and reached a new static VM-object
panic at36,376 SPTM calls. LDG reconstructed `0xf0fffe000c71e500`; the
untagged static pointer should retain the default tag15. A bounded,
single-step probe proved that an AttrIdx4 mapping alone retained tag0,
while architectural STG initialized the granule to tag15. Original tag,
PTE, CPU/debugger context and16 data bytes were restored and verified.

The seed now maps the two writable kernel segments and their PAPT aliases
as tagged memory and enters an88-byte STG loop covering565 pages before
XNU. It preserves x0-x3, writes no payload bytes and never writes TagOffset
or tag-storage RAM directly. Static frame flags and tag-storage references
are initialized consistently. `test-static-mte.py` validates the exact
segments, instruction sequence, literals and unsafe-range rejection.

Guest12 stopped before entry on a15,814,944-byte readback timeout.
The KIS wrapper now uses at most1 MiB per read and hashes all payload bytes.
Guest13 verified the complete upload and advanced past the static-object
panic to515 dynamically tagged pages in11 calls, with zero rejects.

Guest13 then stalled at37,789 SPTM calls in FIQ handling. CNTV_CTL=7 and
CVAL=0 identified a masked expired architectural timer. The carrier's M5
pending predicate discarded IMASK and incorrectly injected it as a virtual
FIQ. Disabling that source in the existing controller advanced XNU to38,007
calls and643 tagged pages in13 calls, with zero rejects. It then panicked:

* ST2G PC `0xfffffe000b99db88`, caller LR `0xfffffe000b86952c`.
* ESR `0x9600004f`, FAR `0xf7fffe187dfaab00`.
* Leaf PTE `0x4600103bc51c683`: AttrIdx0, AP2 (read only).
* Physical frame `0x103bc51c000`, frame-table type25.

Do not infer corruption solely from a page table located inside tag storage.
The observed table at `0x103bc698000` covers tags for unmanaged low DRAM,
which XNU can reclaim. Apple's nearby
[pmap source](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/osfmk/arm64/sptm/pmap/pmap.c)
and [metadata allocation source](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/osfmk/arm64/sptm/pmap/pmap_data.c)
document this policy; these sources are older than the exact12377.161.13
binary. No RAM-layout change was made on this inference.

Carrier14 preserves each CPU's guest ENABLE/IMASK bits separately from the
host mask. Production-function tests include the observed failure, genuine
pending interrupts, compare rearming, CPU separation and unchanged non-M5
behavior. ARM build and scoped RAM chainload passed; guest14 is in progress.

Carrier14 SHA256:

* m1n1.bin: `1decf5111e1e8eab8229c842d26445e5a87aa39601cde0b9e28c4483b98ebb62`
* m1n1-raw.elf: `91a57c0c859e314d83c8effda6a1ad6a55766856a5feb0c1693bbd8aa207b5bc`

The next hardware question is whether the timer correction passes without
manual isolation, followed by the read-only/non-tagged zone mapping fault.
macOS userspace, native N1 tracing and Linux DHCP are still unproven.

## Canonical-tag correction from guest14

Guest14 independently reached37,997 SPTM calls and643 tagged pages in13
calls with zero rejects, qualifying the carrier14 timer fix. Its retagging
panic has the same PC/LR/ESR as guest13, with FAR=`0xfdfffe15dc02d540`.
The read-only zone is `threads_ro`; the input element before random
retagging is `0xf0fffe15dc02d540`, although ordinary kernel memory requires
the canonical tag15.

`probe-canonical-mte.py` single-stepped the exact LDG at
`0xfffffe000b8d0964` against that ordinary read-only page:

* TCR=`0x010800736511a511`: LDG returns `0xf0fffe15dc02d540`.
* TCR=`0x310800736511a511`: LDG returns `0xfffffe15dc02d540`.

Only MTX0/MTX1 differ. TCR, debugger/exception state, PTE and16 data bytes
were restored and checked. No allocation tag or guest payload was written.
Apple's nearby [register definitions](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/osfmk/arm64/proc_reg.h)
enable both MTX bits with MTE. The decisive evidence is the exact-kernel
instruction on live M5 hardware, rather than the older source version.

The seed now sets those two bits only for M5. The temporary static-tag
stub,565 tagged kernel pages and associated storage references were removed;
ordinary mappings and original XNU entry are restored. `test-mte.py` checks
the full M5 TCR value and unchanged non-M5 value. Guest15 qualified this
change:78,557 SPTM calls,19 tag calls,1,027 tagged pages, zero rejects.

Guest15 stopped later on SError, ESR=`0xbe000000`, at
`0xfffffe0009c50590` in the exact AppleT8142PMGR `__TEXT_EXEC` range
`0xfffffe0009c4ba90..0xfffffe0009c5252c`. The asynchronous PC is a context,
not proof of the offending device access. The last protected PMGR writes
in the log target UART_P/UART0 at0x380700398/0x380700470. The diagnostic
handler then faulted while clearing the locked L2C status register and
exited; this host-side problem is now guarded by the CPU feature flag.
Read-only resident counters are in `logs/guest-15/resident-state.log`.
Guest16 uses the same carrier14 and canonical-tag seed, with a PMGR journal.

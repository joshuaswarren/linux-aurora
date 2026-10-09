# Guest33 SEP policy handoff evidence

Guest33 ran carrier32 and exact Mac17,3 macOS 25G76. Cryptex authentication
passed, but the native kernel stopped at HVC4 `0xfffffe000b7bbdd0` with
`SEP Panic: %s\n%s`. The SEP report repeated guest32's `boop/boop` stack
and `[hgggimqmg]` token. The assertion itself remains unidentified.

The passive SEP mailbox journal recorded 126 packets. The native boot
shared-memory message was endpoint 0xfe, low32 0x001c00fe, high32
0x100005cc. Exact AppleSEPBooter::bootSEP at 0xfffffe0009a2f43c encodes
the size in 4-KiB units in bits 16..31 and the slave address shifted by
12 in bits 32..63. Thus 0x1c is a size, not an ordinary endpoint opcode.
The retained AppleSEPSharedMemoryBuffer confirms size 0x1c000 and slave
address 0x100005cc000. No direct packet to a discovered boop endpoint was
seen; the final sbio packet is not established as the cause.

## Retained shared-memory capture

The saved x19 in panic_with_options was at caller FP minus 8, yielding
AppleSEPManager 0xfffffe230ff7b000. Its +0x128 points to shared-buffer
object 0xfffffe1e4609a1b0. The inherited getBuffer implementation at
0xfffffe000abd2310 reads +0x10, giving IOMemoryDescriptor
0xfffffe2ea7e3ad40. The exact getBytesNoCopy implementation at
0xfffffe000bfe9c38 selects the first range at +0x60 for these flags,
giving kernel VA 0xfffffeaab73d4000. Reads used the paused guest's VA
translation; no DART initialization, invalidation, or device write.

The buffer's five 16-byte table records are name u32, capacity u32,
offset u64. OPLA and IPIS payloads begin with an actual-length u32.

| Channel | Offset | Capacity | Payload length |
| --- | --- | --- | --- |
| PNIC / bytes CINP (panic) | 0x4000 | 0x8000 | NUL-terminated report |
| FLAG / bytes GALF | 0xc000 | 0x4000 | 32 |
| ALPO / bytes OPLA (LocalPolicy) | 0x10000 | 0x4000 | 2846 |
| SIPI / bytes IPIS (iBoot) | 0x14000 | 0x4000 | 7922 |
| SCOT / bytes TOCS (test) | 0x18000 | 0x4000 | 32 |

Exact AppleSEPManager::_loadChannelObjectEntries at 0xfffffe0009a25970
loads `lpol` and `ibot` from /chosen/boot-object-manifests. Its descriptor
table at 0xfffffe000c86bf68 retains obsolete mapped VAs after
_loadChannelObjectData at 0xfffffe0009a25ba0 copies into shared memory and
releases both map and memory descriptor. Reading those obsolete VAs gave
unrelated hashes; those reads are not valid policy evidence.

Private captures in logs/guest-33:

- sep-shared-memory.bin: 0x1c000 bytes, retained buffer.
- sep-lpol.bin: SHA256 02d86d6cb68108b40a437c3db67ba6c4707c4cea74e79959e0ad68ffe0a76160.
- sep-ibot.bin: SHA256 eeb3bb8947c29fa331d570df848b1c4d373294319c16808a46df55c2ab587de5.
- resident-lpol.bin: original physical boot policy, 2899 bytes.
- resident.adt: original controller ADT, before guest substitutions.
- sep-panic-message.bin and console.log.

## Confirmed mismatch and next experiment

The staged native LocalPolicy's signed nsih begins 10e0792b6ef95935.
SHA384 of the actual shared IPIS iBoot manifest begins 8649b0378aa3727c.
They differ. The original resident LocalPolicy's signed nsih matches
SHA384(IPIS) exactly, as does original /chosen/boot-manifest-hash.
This establishes a mixed handoff, not yet the exact SEP panic cause.

M5_RETAIN_RESIDENT_BOOT_POLICY=1 is a bounded comparison: preserve the
original measured LP/iBoot pair and boot-manifest identity, while keeping
the native cryptex query mirrors which previously resolved dyld ticket
selection. No signature or root-authentication check is patched. The
default native-policy experiment remains available. The source ticket
for a fully native LP/iBoot pair was not captured locally.

Host tests verify the signed resident nsih against actual IPIS, preserve
the measured pair through ADT serialization, and reject damaged LP,
damaged iBoot and inconsistent resident nsih before guest ADT mutation.
Guest34 passed this hardware comparison: all cryptex images authenticated,
macOS launched system services, and SEP progressed to 681 messages without
the previous panic. The last pause was an ordinary MSR trap about 79
kernel-log seconds after root mount. This supports the corrected handoff
while leaving the precise firmware assertion undecoded. N1 startup then
disabled its PCIe link and removed the control driver, requiring a separate
capture of that reset. Resident Stage1 is unchanged; the same carrier32
was RAM-chainloaded. Linux DHCP remains unresolved.

# EOT physical partition correction (current work)

The earlier assumption that EOT attachment partition IDs equal fragment synchronization-class indices was wrong. Sparse fragment synchronization changes remain valid (independent native fragment oracles). Both the newer dense EOT inputs0,1,2,0,... and the older mixed-format EOT have now been corrected in source. Hardware qualification remains pending.

Authoritative current local UMD was extracted from the system dyld cache with ipsw into native-umd-current/AGXMetalG17P. SHA256267ff1afc6f65ee672dc04d1457cd286f7599fb8146656bb2a8eb03f6c2b21dd. Reproduce selected disassembly with `/opt/homebrew/Caskroom/miniforge/base/bin/python3 native-umd-inspect.py SUBSTRING` (ordinary /opt/homebrew/bin/python3 lacks capstone).

native-umd-framebuffer.asm, Framebuffer constructor:
-216b68940: w13 = framebuffer[0x134c], the imageblock sample stride.
-216b689ac: w24 = (draw_buffer_state >>12)&255, the unmodded tile offset.
-216b689b0: w7 = w24/w13 (UDIV).
-216b689b4..c0: this quotient becomes bits0..7 of the per-attachment EOT key.

native-umd-eot-state.asm, _AGCEndOfTileState::serializeToFlatbuffer:
-216a73580 loads the attachment key word.
-216a73968..216a73a58 serializes its low byte to the attachment's flatbuffer field0 (vtable field4). Thus the field previously called partition is a PHYSICAL stride quotient, not a logical synchronization class.

native-umd-configure-pbe.asm independently computes the same raw offset modulo framebuffer[0x134c] at216b66268..274 for the offset driver argument. Quotient and remainder are complementary. Our admitted compact unspilled layouts have every offset below sample_size_B, so ALL EOT partition IDs are zero, regardless of which logical attachments are active.

New native-mrt-probe.py --remaining-physical-partition-zero-eots fills counts5,6,7; existing eot-1/2/3/4/8-shared provide the rest. All captures terminal. Native sizes for active counts1..8:68,120,164,208,252,296,340,384bytes. Commonfield0=3GPRdwords for all; stagingDbytes=16*count. Conservative allocation8GPRhalves remains valid; staginghalves=8*count. The old dense 8-target EOT1226bytes/D96 is the wrong physical-partition input.

Implemented: the header contains partition-zero native variants1..8; fragment class masks remain separate. A general active-layout mask packs PBEs/physical offsets in active order while preserving logical BG texture/clear slots. CPU layout/loader tests cover all255nonempty masks, including stagingD128 for eight targets. Do not generate42class-sequence variants; that plan was based on the incorrect conflation. The old WBOIT EOT238bytes was reproduced exactly with native formats8/3 and partitions0/1. Changing only its second partition byte to0 produces the now-installed-in-source120-byte witness SHA b3191cbbd673db5fb00d3e744f60ed59befad9bc19a2bf30aa9420effa63df12. It is not installed on the Neo or hardware-qualified. See EOT-AUDIT-FOLLOWUP.md.

Product changes build successfully. Full CPU regressions remain264Pass,2ExpectedFail,1known precompiled-fill failure after the mixed-format correction. Repository tools freshly reproduce all9native inputs, executable texts and resource contracts and compare them against source. No new hardware test or installation has occurred; the last hardware ledger remains807Pass/879reached on c5826185. All build, test, extraction and capture operations are terminal. This document supersedes SPARSE-MRT.md statements about EOT class-sequence mapping and cross-partition handoff requirements for our compact layout. See the audit and follow-up for the remaining limits.

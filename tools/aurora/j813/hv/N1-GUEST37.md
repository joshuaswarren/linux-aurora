# Guest37: N1 firmware boot and native DMA contexts

Guest37 uses unchanged carrier32, exact 25G76 Mac17,3 kernelcache, the
matched resident LP/iBoot pair, and the new opt-in
`M5_NATIVE_BOOT_OBJECTS_PATH=1`. This changes only the guest ADT path to the
captured full macOS boot-object directory after validating its VUID and
signed nsih. Resident Stage 1 and installed boot files are unchanged.

## Hardware result

The former centaurid no-firmware power-off does not recur through the
captured interval. Native N1 firmware boot completes at kernel time
317.971040; at 318.501836 the kernel reports chip reset complete and the
Wi-Fi datapath opening. Both control and alpha report IPC status 2.

Evidence: `logs/guest-37/{console,hv}.log` and chronological decoding of
`logs/msgbuf/xnu_msgbuf_20260909_132754_02.raw` using its JSON receipt's
write index. The initial log ring has wrapped; retain the earlier
`xnu_msgbuf_20260909_132620_01` capture for ROM enumeration.

## Runtime PCI and DART identity

| Function | Product | Role | BAR0 | RID | SID |
| --- | --- | --- | --- | --- | --- |
| 0, ROM | 106b:1900 | Control ROM | 0xb82000000 | 0100 | 1 |
| 0, runtime | 106b:1901 | Control | 0xb82020000 | 0100 | 1 |
| 1, runtime | 106b:1902 | Wi-Fi alpha | 0xb82010000 | 0101 | 2 |
| 2, runtime | 106b:1903 | Beta | 0xb82000000 | 0102 | 3 |

All BARs are 0x10000 bytes. Native IODeviceTree confirms these roles and
runtime assignments. Current PCI configuration confirms control and alpha
identities, MEM enable and BARs. The existing hardware RID-to-SID words at
port0 configuration 0x488028000 + 0x3000 + 4*SID are 0x80010100,
0x80020101 and 0x80030102. Exact 25G76 AppleT8142PCIe code qualifies this
register layout and port0 ADT register index 8. No SID was assigned by the
diagnostic, and no DART initialization or mapping changes were made.

The old prearm followed physical 0xb82000000 across BAR reassignment.
Its later runtime writes belong to beta, despite stale function-0 labels.
They are not native Wi-Fi TX evidence. Refreshing `attach_enumerated`
corrected the live control and alpha attachments.

## Private captures

Files under `logs/n1-dma/` are mode 0600 in a mode 0700 directory:

- `context-fn0-sid1-1788975550742901000.json`: control context IOVA
  0x07010000, 1 completion ring and 13 transfer rings.
- `context-fn1-sid2-1788975550871579000.json`: alpha context IOVA
  0x04610000, 234 completion rings and 192 transfer rings.
- `context-fn1-sid2-1788975689230287000.json`: alpha context plus 16
  retained message-TR payloads, including ring creation commands.

The bounded reader validates all translations and physical DRAM ranges
before fetching payload fragments. It selects the context register layout
by verified product: control 1900/1901 use 8050/8054; alpha 1902 uses c/10;
beta 1903 is rejected. The host attachment/layout regression passes.

TR25 head/tail are still zero in this pause. Its create command is no
longer among the 16 retained message descriptors. Those slots are not
necessarily pending commands or chronological history; no TR25 allocation
address is inferred from neighboring buffers.

`stop-n1-tx.py` arms a one-shot WSYNC observer at the revalidated alpha
BAR + 0x1030, requiring the current native RID 0101 to SID 2 association
and IPC-ready status. Its shell runs before m1n1 forwards the original
write. The observer does not write the doorbell itself.

## Native TX and DHCP capture

The synchronous observer caught alpha BAR+0x1030 write 1 at native EL0 PC
0x19d3b4a8c, before m1n1 forwarded the original write. The pending native
submission was on **TR43**, not TR25. Its creation message supplies DMA
base 0x05914000, 2048 slots, completion ring 84, doorbell 18, flags 8,
16-byte inline footer and extension `00 00 06 03 ff ff ff ff ff ff`.

The first packet is Ethernet/IPv6 UDP 5353 mDNS. A bounded recorder then
captured **DHCP Discover**, Ethernet broadcast, UDP 68 -> 67, 342 bytes,
XID b139cb04, at TR43 slot 7. Descriptor:

```
0356010000589d05000000000700000000000200000000000000000000000000
```

Decoded: flags 3, length 342, payload IOVA 0x059d5800, tag 7, chain 0,
footer first word 0x20000, second word 0. IPv4 and UDP checksums are
nonzero. Exact 25G76 AirshipDataPacket::prepare at 0x18090faf8 clears
the metadata and writes `(argument & 7) << 15`; value 4 produces this
0x20000 field. Its semantic argument is not yet identified.

Private evidence:

- `logs/n1-dma/tx-stop-1788976259676680000.json`: first TX register snapshot.
- `logs/n1-dma/context-fn1-sid2-1788976397558107000.json`: first descriptor and mDNS payload.
- `logs/n1-dma/tx-event-1788976673375842000.json`: DHCP event and native ring configuration.
- `logs/n1-dma/context-fn1-sid2-1788976673731647000.json`: DHCP descriptor.
- `logs/n1-dma/context-fn1-sid2-1788976673864818000.json`: DHCP payload.

At this experiment, Linux created its diagnostic TR25 with flags 0 and footer
`6 << 27` plus expiry 2000000. These are observed differences, not yet
proof of which change is required. The broadcast extension agrees with
native TR43. Ring numbers themselves are not a demonstrated bug.

## Clock and sleep limitations

After association, macOS requested a Wi-Fi firmware abort for a fatal
invalid RX ingress timestamp at kernel time 353.715703. The tracing session
had accumulated 1981.772197682 seconds of stolen architectural time while
N1's hardware clock continued. `hv_set_time_stealing(False, True)` cleared
the offset; subsequent native Wi-Fi recovered and SSH returned build
25G76 and en0 address 192.168.68.56. This qualifies native networking
briefly, but a DHCP ACK/lease receipt was not fetched before the next fault.

The guest then entered idle sleep at kernel time 2683 and hit a dart-sep
SID exception, ERROR_SID_SUMMARY 0x100, address 0x100005d0000. The preserved
kernel log is `logs/msgbuf/xnu_msgbuf_20260909_140737_03.{raw,json}`; fatal
PC is 0xfffffe000b7bbdd0. Guest37 is halted. Do not continue that panic.
Guest38 should retain bootstrap's existing time-stealing-disabled default
and launch a temporary `caffeinate` process once SSH becomes available.

## Remaining qualification

Native DMA descriptors and a DHCP Discover payload were proven in guest37.
Guest38 subsequently captured a stable native lease. The later Linux
crossover isolated preparation value 4 and completed DHCP; see
[Linux packet evidence](../N1-DATA-RINGS.md) for current qualification.

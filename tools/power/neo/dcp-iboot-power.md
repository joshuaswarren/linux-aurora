# J700 firmware display power command

Offline analysis of 25F84 AppleDCP-1041.120.7~1815-t8140dcp.RELEASE.
Original `dcp.macho` SHA256
`091e9d9320818360d422c938bde5e4fd5c4e83ccb6b44e2134fac907b975ae9f`;
analysis-only resolved-pointer copy SHA256
`97f0d0a9961c6d7a067356c18b8c9a9e22cfc2ef91a9b68572a7999fc576c8aa`.
Neither image was modified or installed during this investigation. Captures:
parent workspace `artifacts/neo-suspend/wifi-idle/dcp-iboot-*.log` and `.dis`.

The existing m1n1 `src/dcp_iboot.c` describes endpoint 0x23, EPIC service
`disp0-service`, subtype 0xc0. Its command header is four little-endian u32
words: operation, total byte length, zero, zero. Power operation 2 has one
payload byte, 0 or 1, hence total length 17. This is confirmed against the
actual J700 firmware, rather than assuming an older firmware ABI:

- 0x42e07f4 dispatches opcode 2, requires more than the 16-byte header and
  checks supplied length against the header's total length.
- 0x42e15b8 logs `setPowerRequest`, reads the byte at request+0x10 as boolean,
  locates the display instance, and calls 0x41c3a88 through its work loop.
- 0x41c3a88 checks current display power at pipeline+0x4004, then schedules
  0x41f2d94 if the requested state differs.
- 0x41f2d94 builds the internal power request and invokes the pipeline's
  virtual method at +0x768.

This establishes the request format and dispatch path. It does not prove
that the current inherited display endpoint is ready for a new host, that
all display state will survive power-on, or how much power this saves.
No command was sent. Full DCP CPU power gating is a separate transition.

The missing integration is AFK/EPIC transport ownership. Both existing m1n1
and Linux AFK implementations allocate a ring buffer on GETBUF, then command
TX/RX buffers using the owning DART mappings. The current retained-DCP helper
owns only management/OSLOG and intentionally refuses new DMA allocation.
Do not advertise arbitrary physical RAM as an IOVA, recycle an apparently
unused inherited buffer, or reset the live display DART to get past this.
First establish a reserved, mapped allocation or a properly retained DART
owner, then qualify endpoint startup and a read-only display query before
bounded off/on commands. Restore a known frame and verify power separately.

The archived region-50 capture contains initialized DATA at the expected
mapping, but extrapolating that linear mapping into heap addresses yielded
zeros for the display-service object. This is not proof that the object is
absent. Resolve the firmware VA through its actual mappings before further
object inspection.

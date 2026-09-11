# M5 native graphics resource reference

The 2026-09-11 native macOS 25G76 reference captures a 256x256 triangle on
Apple M5 through both TA and fragment SKSM queues. This is a reference for
the Linux graphics implementation. Linux has a verified compute triangle;
Linux vertex/fragment rendering, Mesa admission and DCP presentation remain
unqualified.

## Linux register codec

`drivers/gpu/drm/asahi/m5_render.rs` now constructs the M5 main and partial
register programs from shared render resources and explicit M5 controls.
Every register number, value and position matches the captured reference:
82 TA writes, 90 fragment writes, and 18/25/12 partial store/resume/load
writes (227 total). The kernel compiles with the module included.

M5 adds a second store-pipeline pair to each fragment program. Its tiler
block words differ from G17P: the reference boundaries 12, 8 and 4 encode
as `0x40200c` in ten-bit fields. Native reads these words from host descriptor
`+0x1208/+0x120c` for both the ordinary and shadow tiling registers. The
descriptor copies them from user command `+0x108/+0x10c`. The wider packing
model is checked for rectangular dimensions on the host; dimensions beyond
the captured reference have not yet been qualified on M5 hardware.

Resource IDs and per-stage keys are caller inputs. The USC FList identifier
is sign extended as in the native generator. PTC maxima use the native zero
fallback of `0x60`. Geometry fields reject overflow and overlapping flag
inputs, and TA dispatch bit zero is supplied separately for the two phases.

Run `python3 drivers/gpu/drm/asahi/tests/test_m5_render.py` for six tests,
including the complete native register vectors and ownership/geometry cases.
The shared memory-backed policy and render-counter tests also pass. Three
older G17P full-suite expectations reproduce on the unmodified parent.

The codec is not yet called by a Linux graphics submission. Descriptor
construction, owned graphics buffers and scheduler submission are the next
integration steps. The existing Linux compute-triangle proof remains the
current GPU execution milestone.

## Hardware evidence

The reference produced 18,432 foreground pixels, 47,104 background pixels and
zero unexpected pixels. It independently checked 63,234 pixels away from
rasterization edges. The captured work contains 82 TA register writes and
90 fragment register writes in GPU context 3.

A pre-publication, table-only survey identified 334 distinct resource pages.
All were within the first 4 MiB of the user GPU windows at `0x1000000000`
and `0x10000000000`. The subsequent resource capture saved 351 objects,
including all 334 pages (5,472,256 bytes), and five page tables. Two reads
of every data object agreed; translations and page tables remained stable.
These are pre-submit program/data resources, not post-render pixel results.

Holding Metal objects after completion did not preserve most GPU mappings.
The retained process had only two pages in these windows after its draw.
Capture resources before publication, rather than relying on object lifetime.

The physical read path remains the existing DRAM-only `M5GPUCapture` reader.
The survey does not follow the firmware root or accept GPU block mappings.
Full capture is limited to 384 resource pages and a 16 MiB aggregate budget
for repeated data views. Page-table reads have separate bounds and receipts;
the data-view budget is not a total transport-traffic accounting limit.

## Native descriptor fields

These fields were correlated with captured host descriptors and the pinned
25G76 generator. Values are per-job inputs, not hardware constants to copy
into Linux. In particular, the hardware buffer identifier needs an owned
Linux allocation, just as the compute UMA pool selector does.

| Stage | Emitted register | Host descriptor field | Transformation |
| --- | --- | --- | --- |
| TA | `0xd499` | `+0x1120`, u64 | direct |
| TA | `0xa5a1` | `+0x1128`, u64 | low 32 bits of `(source or 0x60)` shifted 32, OR `0x400020` |
| TA | `0x14308` | `+0x1400`, i32 | sign extend to 64 bits |
| Fragment | `0xd491` | `+0x7e8`, u64 | direct |
| Fragment | `0xa5a9` | `+0x7f0`, u64 | low 32 bits of `(source or 0x60)` shifted 32, OR `0x400020` |
| Fragment | `0x14048` | `+0xb60`, i32 | sign extend to 64 bits |

The TA generator starts at `0xfffffe00089ebd54`; the fragment generator at
`0xfffffe00089db0e8`. The signed loads occur at `0xfffffe00089edb80` and
`0xfffffe00089dd314`. Neo's render implementation identifies the analogous
fields as the USC FList hardware buffer ID, separate from the TA parameter
buffer ID. Do not replace these fields with the compute pool ID by analogy.

The fragment tile-state register `0x16068` also differs from Neo:

- X tile count minus one occupies bits 42..51; Y occupies bits 52..61.
- Four utile bits occupy bits 38..41.
- Native `bfi` instructions at `0xfffffe00089dcbb8`, `0xfffffe00089dcbbc`
  and `0xfffffe00089dcb98` establish these positions.
- The controlled single-layer, single-sample draw uses utile configuration
  `0xa000`, tile state `0x701ea00003717f`, then a final `0x40000` write.
  Additional native flags must be derived before generalizing this witness.

## Reproduction and verification

Use the existing M5 native HV owner, a prepared SSH connection and the
stdin gate in `m5-triangle-reference.m`. Arm `trace-m5-graphics-submit.py`
only after `M5_TRIANGLE_READY`, excluding WindowServer queues 2 and 3.
`M5_GRAPHICS_CAPTURE_CONTEXT=map` surveys translations without resource
reads; `=1` performs the bounded resource capture. The selected context
must match `M5_GRAPHICS_CONTEXT_ID` (default 3).

`verify-m5-graphics-capture.py --ta TA_DIR --fragment FRAGMENT_DIR
--native-result RESULT_JSON --output REPORT_JSON` verifies object hashes,
saved UAT translations, alias consistency, six descriptor/register
correlations, the tile-state witness and the separate native pixel result.
Its report explicitly records that Linux graphics and DCP are unproven.

Seven page-survey tests pass. Negative provenance checks reject a changed
resource byte, changed page-table byte, inconsistent leaf receipt, unstable
context flag and a failed native pixel result. Raw host descriptors and
resource dumps stay in private capture artifacts; they are not embedded
as Linux addresses or published as a general-purpose graphics replay.

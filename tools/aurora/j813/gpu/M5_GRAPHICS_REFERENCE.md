# M5 native graphics resource reference

The 2026-09-11 native macOS 25G76 reference captures a 256x256 triangle on
Apple M5 through both TA and fragment SKSM queues. This is a reference for
the Linux graphics implementation. Linux now has verified compute and
TA/fragment triangles, including three consecutive TA/fragment renders with
fresh output and completion stamps. Mesa admission and DCP presentation
remain unqualified. The sections below record the successive bring-up stages.

## Completed native ownership capture

The narrowed fragment capture completed without capture errors on carrier33,
and the same native command produced 18,432 foreground and 47,104 background
pixels with zero unexpected pixels. The downloaded 262,144-byte RGBA image
independently has exactly those two colors and counts. All 31 saved objects
passed length, SHA-256, repeated-read stability and mapping-stability checks.

The PM owner has three 512-KiB blocks, 48 allocation units of 32 KiB each,
and a 112-KiB page list. Its high and user views contain identical page IDs.
Block-list entries have an eight-byte stride: first IDs `0x11`, `0x22`,
`0x33`, each followed by zero. The capacity fields are 1,650 blocks and
26,400 allocation units, consistent with 16 units per block.

The USC directory contains 9,728 consecutive 4-KiB allocation units in
19 blocks of 2 MiB, with 32-KiB guards. Its first block is at user VA
`0x7000420000`, while the directory and free-list views start at
`0x7000000000` and `0x7000408000`. The earlier compute owner uses 14 blocks
starting at `0x7000448000`; reusing those constants would overlap the
graphics layout or supply the wrong pool size. The native hardware slot
is 1 in this witness and must be replaced by a slot owned by the Linux job.

The pre-submit PB descriptor row selected by hardware ID4 is still zero.
This observation does not establish its later firmware publication protocol.
After completion, one PM-related owner address was already unmapped even
though the reference process was held. A process-level hold is insufficient
to prove that every internal driver allocation remains alive; post-completion
reads must check translations again. Linux job objects remain retained until
their firmware lifetime is independently established.

## Linux register codec

The channel configuration codec now matches both captured 134-byte TA and
fragment tag-15 records byte-for-byte. Four channel tests and four SKSM
tests pass. The AddKicks codec selects the stage explicitly, and the
compute wrappers preserve their existing record format.

Three independent channel fields matter: primary selector `+0x22` is
0 for TA and 1 for fragment, secondary selector `+0x5e` is 2 for both, and
the separate `+0x26/+0x2a` state update carries an enabled flag with value
zero in this reference. The TA record also enables the second event tuple:
PM state address, timestamp, hardware PB index and a separate manager ID.
The native writer at `0xfffffe0008a249ac..0x8a249e4` obtains those last two
words from TA descriptor `+0x1218` and PM manager `+0x79c` respectively.
They must come from retained Linux owners when submission is implemented.

`verify-m5-channel-config.py --source m5_channel.rs --ta TA_DIR
--fragment FRAGMENT_DIR` checks stable hashed records against the actual
Rust encoder. Its result is a codec qualification, not a graphics draw.

The graphics memory owner can create firmware aliases of its existing
user pages before context publication. It verifies matching physical
translations and returns the alias owners to the eventual job. This
supports shared PM page lists and scene counters without allocating a
second unrelated backing store. Runtime graphics submission was still absent
at this codec stage; the later hardware qualifications are recorded below.

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

Run `python3 drivers/gpu/drm/asahi/tests/test_m5_render.py` for nine tests,
each run with Neo diagnostic overrides disabled and fully enabled, including
the complete native register vectors and descriptor ownership/geometry cases.
The shared memory-backed policy and render-counter tests also pass. Three
older G17P full-suite expectations reproduce on the unmodified parent.

The codec now builds TA/fragment descriptor bases with M5 register counts,
partial-program offsets, checked self aliases and metadata-derived work keys.
Shared G17P encoders keep their diagnostic overrides in the G17P wrappers.
The refactor preserved successful generic/cold descriptor bytes across ten
override masks, three dimensions, two PM settings and varied contexts,
ordinals and state words. Runtime/completion tails remain unfinished; these
bases must not be submitted as complete commands.

## Private program resources and Linux ownership

`build-m5-graphics-bundle.py --ta TA_DIR --fragment FRAGMENT_DIR
--native-result RESULT_JSON --output PRIVATE_BUNDLE` verifies the capture,
then packages only the 334 user GPU pages. The version-1 bundle preserves
108 read-only and 226 writable mappings, including 16 untouched output
pages. It contains no captured page tables or kernel command objects.
An aligned-word audit rejects direct captured kernel/physical pointers;
it does not prove absence of every possible packed or encoded pointer.
The generated 5,477,664-byte blob stays private and outside Git.

`m5_graphics_bundle.rs` validates the bounded format before allocation:
256x256 geometry, context 3, two user windows, sorted unique addresses,
permissions, exact record lengths, a transfer checksum and complete
sentinel-filled output. `m5_graphics_memory.rs` copies accepted bytes into
new Linux-owned WC allocations with corresponding GPU RO/RW permissions.
The constructor leaves roots unpublished. Its firmware name is
`asahi/m5-25g76-graphics.bin`; the Asahi Kconfig selects the Rust firmware
loader abstraction. There is no runtime caller yet.

Run `python3 drivers/gpu/drm/asahi/tests/test_m5_graphics_bundle.py` for five
synthetic tests. Optional `--bundle PRIVATE_BUNDLE --native-rgba RGBA_FILE`
checks the actual private package and the independent pixel checker against
the native result. The reference passes with 18,432 foreground pixels,
47,104 background pixels and zero errors among 63,234 off-edge checks.
This validates the checker against native output, not Linux rendering.

The kernel builds with these modules included. Completing M5-specific
runtime fields, owned TA/USC pools, queue publication and retirement is next.
The existing Linux compute-triangle proof remains the execution milestone;
the new graphics mapping owner has not yet run on hardware.

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

`M5_GRAPHICS_CAPTURE_OWNERS=1` adds bounded channel configuration, PM and
USC owner views. `M5_GRAPHICS_CAPTURE_STAGES=fragment` limits instrumentation
to the fragment submission; the default remains `ta,fragment`. Use the
narrow stage when a second TA pause would hold up an already-published
fragment job. PM field `+0x7c` is not the block-table byte extent: requesting
that many bytes reached an unmapped page. The capture now explicitly reads
only the first mapped block-table page.

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

## Linux graphics submission diagnostic

Diagnostic mode 12 now constructs Linux-owned TA and fragment resources,
paired priority-2 SKSM queues, a fragment dependency on the TA completion
stamp, and shared context mappings. The RAM payload builder includes the
private local `asahi/m5-25g76-graphics.bin` bundle when available. After a
fresh GPU diagnostic boot has released its console, run
`M5_FW_TRACE=0x203 python gpu/run-m5-queue-install-probe.py 12`.

The first hardware run on 2026-09-11 consumed both outer queue records
(`[[1,1,1],[1,1,1]]`), but neither inner queue advanced
(`[[0,0,3],[0,0,3]]`). All 65,536 output pixels retained their sentinel
value. The transaction returned `EIO`; both firmware stop requests
succeeded, and resource owners remain retained until reboot. The console
runner correctly reported failure even though module insertion returned
zero. This is a submission diagnostic, not a verified Linux graphics draw.

Separating initial installation from drawing did not resolve the legacy
outer-channel stall: both firmware registration slots remained empty.
Routing both stage-tagged command lists through the common CL_2 outer ring
did resolve registration. Hardware consumed two outer records and retired
one setup command on each inner queue. Both queue slots have the exact
owned ring addresses and share the scheduler control, with header
`0x02040000` and queue bitmap `0xc00`.

The GFX firmware queue slot's final word is the stage, not priority:
handler `0x12240` loads command `+0x22`, then `0x1241c` stores it at slot
`+0x20`. The observed TA and fragment values are 0 and 1; compute uses 2.
The Linux guard now checks each stage explicitly. The first routed run
stopped before drawing because that guard still expected the compute value.

Mode 12 now verifies installation before publishing the user context and
draw commands. The draw adds two more records to the shared outer ring and
three commands to each inner queue. Queue retirement requires outer counters
`[4,4,4]` and both inner counters `[4,4,4]`. The original zero-pending check
was incorrect: firmware compares completed stages against a host-owned
submitted total. The completion-list correction below additionally checks
the scheduler, host stamps and firmware notifications. Do not unload the
module or retry firmware startup within a retained failed boot.

## Verified Linux TA/fragment framebuffer

The routed draw on 2026-09-11 produced the complete expected 256x256 GPU
framebuffer: 18,432 foreground pixels, 47,104 background pixels, no untouched
pixels and no incorrect pixels. Both command lists retired with counters
`[4,4,4]`. TA and fragment dispatch/return traces were observed, with no GPU
recovery. The kernel probe still reports `EIO` because its separate pending
and stamp observations remain `[2,0,0,0,0]`; this is not a claim that the
whole diagnostic or completion-accounting implementation passes.

After both firmware stop requests, the bounded retained-RAM reader captured
28 objects through 192 physical pages with no failed objects. The actual
262,144 framebuffer bytes match the native macOS reference byte-for-byte:
SHA256 `ce4e527e102b5e2f3f0585a6c045bc3eca14e5d225da0a8f3dc24b53f8c60a13`.

`capture-m5-linux-compute-state.py --after-graphics-draw INVENTORY CAPTURE_DIR`
requires the matching Linux mode12 result and retired command queues before
reading RAM. `verify-m5-linux-graphics.py CAPTURE_DIR OUTPUT.png` checks every
capture hash and independently verifies every pixel against triangle edge
equations. Its PNG uses captured bytes with 2x integer enlargement; it does
not render a substitute triangle. Truncated input, changed colors and wrong
geometry are rejected, and the PNG round trip preserves every source pixel.

This first controlled TA/fragment draw established correct pixels while
completion accounting remained unqualified. The correction and its later
hardware result follow below. The image is an offscreen GPU framebuffer;
DCP scanout and an accelerated desktop require separate verification.

## Shared graphics completion list

Both retained native channels have the same `channel+0x10` job-list pointer,
`0xfffffc2000000048`. Linux initially allocated one list per channel. The
TA stage linked the shared scheduler into its list, then the fragment stage
checked a different, empty list. The retained scheduler consequently held
both completed stages (`+0xc == 2`) but stayed linked (`+0x94 == 1`), with
both software stamps zero.

Pinned 25G76 GFX firmware SHA256
`40f8c4eccadff17ee02b8f3afb480f2e0980a6a3d32f88f69fb32f864e247b0e`
establishes the ownership: `0x17c00` skips relinking a scheduler already on
a list; `0x17544..0x17568` compares completed stages with the submitted
total; `0x1763c..0x1765c` unlinks a finished scheduler. The total is not
decremented. `0x17a68` and `0x17ae0` write each stage's first stamp pointer,
then `0x17b98` publishes the accumulated stamp-event mask. The second stamp
pointer remains an observation, not an independently required signal.

Linux gives both channels one uncached list at
`0xfffffc20c0020000`. Host-visible submitted totals and stamps are uncached
at `0xfffffc20c1080000`, `+0x4000`, and `+0x8000`, separate from cached
descriptors. Completion requires total/completed `2/2`, scheduler unlinked,
an empty list with its own tail sentinel, current software stamps `0x100`
for qids10/11, and both primary-firmware event bits. Queue retirement and
correct framebuffer pixels are still required.

The 2026-09-11 hardware run passed these gates and returned `Ok(())` from
mode12. Firmware emitted the scheduler-unlink traces `0x10b`/`0x10d`, TA
and fragment stamp traces `0xa`/`0xc`, and a primary type1 event mask `0xc00`.
The final state was submitted/completed `2/2`, linked `0`, empty list,
software stamps `[0x100,0x100]`, and retired outer/inner queues. Both firmware
stop requests then succeeded. The second stamp words remained zero, as the
completion handler's first-pointer stores predicted.

A retained capture of 30 objects through 200 physical pages independently
confirmed the cleared scheduler/list, both current stamps, both firmware
notification bits and all 65,536 expected pixels. Its framebuffer SHA256
is the same native-reference hash above. `verify-m5-linux-graphics.py
--require-completion CAPTURE_DIR OUTPUT.png` rejects missing stage signals,
stale stamps, a linked scheduler, nonretired queues and recovery records;
it also rejects the earlier correctly rendered but stranded capture.

This qualifies completion for one controlled offscreen pair. Repeated
rendering, general Mesa/OpenGL submission and DCP/XFCE remain unverified.

Use `--after-graphics-draw --graphics-shared-list` with the retained RAM
reader for this layout. Omitting the second flag preserves the earlier
draw's capture addresses.

## Consecutive Linux frames

The 2026-09-11 repeat test completed three TA/fragment pairs in one context.
Before frames 2 and 3, Linux erased and read back all 65,536 output pixels.
Each frame produced 18,432 foreground and 47,104 background pixels, with
zero unexpected or untouched pixels and zero incorrect checked pixels.
Both queues retired after each pair; the scheduler unlinked and the shared
list became empty. Firmware advanced the two software stamps through
`0x100`, `0x200`, and `0x300`, with fresh completion event bits for each pair.
Mode12 returned `Ok(())` and both firmware stop requests succeeded.

The repeat path appends fresh queue and SKSM entries, restores only retired
descriptor/scene records, and preserves the context and PB/USC allocation
owners. It retains old software stamps until firmware writes the next
generation. This test does not qualify ring wrap, concurrent clients or
arbitrary shaders: only three sequential generations of the controlled draw
have been tested, although the diagnostic accepts 1..64 frames.

The first repeat candidate overflowed the kernel stack during setup, before
any draw. The corrected implementation allocates the retained firmware graph
on the heap and completes base allocation before graphics construction.
It separates the descriptor builders and stores scene templates on the heap.
The boot initialization frame decreased from 5,264 to 288 bytes; hardware
then completed all three draws. Eleven descriptor tests pass with the change.

The final retained capture contains 30 objects from 200 physical pages,
with no failed objects. Independent verification checks all 65,536 pixels,
the empty shared list, scheduler total/completed `2/2`, both retired inner
queues at position 10 and both stamps `0x300`. Its framebuffer is an exact
match for the native-reference SHA256 above. The verifier rejects this
capture when asked to qualify frame 1.

Run `run-m5-queue-install-probe.py 12 --frames 3` with the qualified trace mask
`M5_FW_TRACE=0x203` after a fresh diagnostic boot. Capture with
`--after-graphics-draw --graphics-shared-list`, then verify with
`verify-m5-linux-graphics.py --require-completion --frame 3 CAPTURE OUTPUT.png`.
The final capture proves frame 3; the ordered driver log and output-reset
checks establish the preceding draws. Mesa/OpenGL and DCP remain separate
unverified integration steps.

## Application context ownership

M5 now leases application slots 3..63 independently of the legacy binding
allocator. Each lease retains the submitting VM as its lower root and a
distinct empty upper root. It claims the slot before allocating or publishing
roots, rejects kernel or foreign VMs, and retracts only its exact root pair
before invalidating the ASID and releasing the owners. The legacy allocator
cannot overwrite M5 leases. Mapping changes invalidate every published M5
context referring to that VM, including newly mapped pages whose missing
translations may have been cached.

The 2026-09-11 context-4 hardware test rendered three consecutive controlled
triangles after a fresh RAM boot. Both stage descriptors and both queue
configurations followed the selected context. Each frame retired both queues,
unlinked the scheduler and advanced completion stamps through 0x100, 0x200
and 0x300; output was erased and checked before the second and third draws.
Both firmware stop requests succeeded.

The independent retained-state verifier confirmed distinct valid lower/upper
roots tagged with ASID4, context4 in both stage descriptors, all 65,536 exact
pixels, and frame3 completion. The capture contains 30 objects from 200
physical pages with no failed objects or device-memory reads. Its pixel hash
matches the native reference above. Asking the verifier for context3 or
context5 rejects this capture.

Use `--context 4` with all three helpers: the mode12 probe, the retained RAM
capture and the pixel/completion verifier. Ordinary application queues will
allocate their slot automatically; selecting a fixed slot is a diagnostic
option. The hardware-tested module SHA256 is
`38997e20c241ebaf85cff306587f25e01c06419df0ceaf2db4bd33bcf68d346e`.

This proves the new context plumbing for the controlled native-program draw.
It does not qualify concurrent clients, dynamic map/unmap invalidation under
load, general Mesa submissions, glxgears or DCP/XFCE. Those remain separate
application-runtime and presentation work.

## USC allocation in the application reservation

The graphics USC directory, freelist and all nineteen 2 MiB blocks now use
free addresses within the VM_CREATE driver reservation. Firmware pointers,
directory entries and UMA records are derived from the owning mappings.
Each directory/block reserves two unmapped 16 KiB guard pages in the same
allocator node; the freelist reserves one. A later allocation cannot consume
those gaps. Existing callers requesting zero or one guard page retain their
previous reservation sizes.

The diagnostic graphics VM now reserves the upper 32 GiB below the usable
42-bit VA limit, matching Mesa's VM_CREATE policy. Its captured application
program pages retain their original VAs outside that interval. The 2026-09-11
hardware run placed the USC directory at `0x3f7ffff8000`, the freelist at
`0x3f800400000`, and all 38 MiB of block backing within
`[0x3f7ffff8000, 0x3ffffff8000)`. Three consecutive context4 draws produced
exact pixels, fresh stamps and retired queues. Both stop requests succeeded.

A complete retained capture independently verifies all 65,536 pixels,
frame3 completion, directory/UMA agreement, nineteen nonoverlapping blocks
within the reservation, and page-table walks ending in invalid entries for
all 41 guard pages. It contains 32 objects read through 233 physical pages,
with no failed objects or device-memory reads. The module is 3,832,368 bytes,
SHA256 `b00b842e29975bfc6bfaf3927221a9ce7853ab0e65e6ac39b5f76f5000193a03`.

The post-draw UMA free-ring cursors advance while their distance remains
9,728 units. The retained cursor pair was consumer `0x8d0`, producer `0x2ed0`;
earlier successful fixed-address captures show the same distance with
different cursor values. The directory entry just after the initial
population can therefore contain a returned owned unit. Construction-time
zero cursors and a zero sentinel are not post-draw invariants. The verifier
checks the returned population and rejects foreign returned addresses.

Use `--graphics-usc-pool` on the retained-state reader and
`--require-usc-pool` on the pixel/completion verifier. Eleven synthetic fault
tests cover stale UMA pointers, missing units, malformed directories,
foreign return addresses, out-of-reservation storage, mapped guards,
foreign roots, broken walks and missing guard evidence. The earlier
fixed-address capture is rejected by the relocated-pool gate.

One initial serial read had malformed Base64 and its capture was rejected.
A complete reread of the same retained state passed; the GPU was not rerun.
This result qualifies dynamic USC allocation for the controlled native
program. Other private render resources, persistent application runtime,
DRM submission/fences, general Mesa rendering and DCP/XFCE remain unfinished.

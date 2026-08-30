# 25G83 DCPLink A401 consumer contract

## Result

The last proven `A401` boundary is the firmware's fifth synchronous `D563`
callback, `DispPerfState`. Linux writes output `1`; the corresponding raw
`0x42` acknowledgement is admitted to and consumed from the ASC A2I FIFO. The
firmware nevertheless never sends `D120` (`start_hardware_boot`) and never
returns the outer `A401` reply.

The transport result rules out an AP mailbox drop. It does not by itself prove
whether the consumed word reached the DCPLink stream action, woke its waiter,
or returned far enough to release the five temporary OSNumber objects. Those
operations all precede `D120` and none has another AP-visible callback. The
strongest exact statement is therefore that execution does not cross the
post-property UPPipe return, not that it necessarily remains in the fifth
wait.

## A401 dispatcher

All addresses are root-task VM addresses in the exact 25G83 binary documented
in `README.md`.

1. `0x041b5890` is the generic inbound EP37 RPC method lookup.
2. The A4xx branch dispatches through `0x041f87bc`.
3. Big-endian tag `A401` (`0x41343031`) selects jump-table entry 1 and wrapper
   `0x041fe888`.
4. `0x041fe888` requires an output size of at least four bytes. It reads the
   instance index at request-context offset `0x38`, selects the object from
   `0x0496b768 + index * 0x30`, gets the gate/workloop through object offset
   `0x2758`, and synchronously runs action `0x041f43b8` through
   `0x0403209c`.
5. `0x041f43b8` invokes object vtable slot `0x38` and maps the returned bool to
   the RPC status.
6. The UnifiedPipeline vtable address point is `0x0451b9c0`. Its slot `0x38`
   is an authenticated chained pointer to `0x04264c38`,
   `UnifiedPipeline::start_ios_gated`.
7. When the gated action returns successfully, `0x041fe908` writes output bool
   true. Only then can the generic link layer emit outer reply `0x42`.

For these chained ARM64e vtable entries, the callable target is
`0x04000000 + (raw_pointer & 0xffffffff)`.

## Path to the five D563 callbacks

`start_ios_gated` at `0x04264c38` performs the startup property/service work
seen as callbacks 0 through 164. The final visible path is:

- `0x04265454`: `D100`, which nests A386 and its callbacks.
- `0x0425656c`, `0x04248ab8`, `0x04248b14`: service-match sequence that nests
  A131/A132 and produces D206/D207.
- `0x042654b4`: virtual call through UPPipe slot `0x378`.
- All three H17P UPPipe vtables (`0x04509a28`, `0x0450bb90`, and
  `0x0450d200`) map slot `0x378` to `0x042abba0`.

The relevant tail of `0x042abba0` is exact and linear:

| Call site | Property sent through UnifiedPipeline slot `0x170` |
| --- | --- |
| `0x042ac02c` | `MaxVideoSrcDownscalingWidth` |
| `0x042ac060` | `VideoClock` |
| `0x042ac094` | `PixelClock` |
| `0x042ac0c8` | `DisplayClock` |
| `0x042ac0fc` | `DispPerfState` |

UnifiedPipeline slot `0x170` targets `0x041f7a08`, the `D563` thunk. The
instructions after the fifth call cannot run until that call returns.

If it returns, `0x042ac100..0x042ac19c` releases the five temporary number
objects. The optional object at pipeline offset `0x59a0` is then queried
through vtable slot `0x70`. Its false/null paths return immediately. Its true
path tail-calls H17P UPPipe slot `0x1b0`; that reaches `0x0423c024`, then
UPPipe slot `0x348`. Every H17P UPPipe vtable maps slot `0x348` to
`0x0423ad50`, which is only `mov w0, wzr; ret`.

There is therefore no post-D563 wait, endpoint, property, or shared state in
this virtual call. After it returns, `start_ios_gated` continues at
`0x042654b8` and sends `D120` at `0x042654f4`. The observed log has no `D120`,
which proves execution has not crossed the containing UPPipe call at
`0x042654b4`. Absence of `D120` alone does not distinguish the fifth stream
wait from the five immediately following object releases.

## D563 RPC return gate

`0x041f7a08` sends tag `D563` (`0x44353633`) with 76 input bytes and four
output bytes through `0x041b7aec`. The call is packaged by `0x041b4e90`; its
gated action reaches the synchronous RPC caller at `0x041b5138`.

The caller sends the EP37 callback at `0x041b53e0`, then waits as follows:

- `0x041b542c`: wait on the stream condition at stream offset `0x58`.
- `0x041b5438`: load the 16-bit response at stream offset `0x48`.
- `0x041b543c`: mask it with `0xc3`.
- `0x041b5440`: compare with `0x42`.
- `0x041b5444`: branch to success at `0x041b552c` only on equality.
- `0x041b552c`: read the remote error value beginning at stream offset
  `0x4a`, then copy output and release the shared stack on success.

The firmware reply ingress is equally direct:

1. `0x041b70d0` receives EP37 words.
2. A raw word with low type 2 and control bits `0x40`, such as `0x42`, takes
   the reply branch at `0x041b73ec`.
3. `0x041b7830` maps the word's stream selector and side bit to one of eight
   stream objects. The DCPLink object is rooted at VM address `0x04963d80`.
   Local streams begin at object offset `0x78`; remote streams at `0x218`;
   each stream object is `0x68` bytes. The message selector and the object's
   local ID are both zero for these callback acknowledgements, so the selector
   equality branch chooses local stream 0 at `0x04963df8`.
4. `0x041b76e0` enters that stream's gate.
5. Action `0x041b7804` stores the full raw acknowledgement at stream offset
   `0x48` (`0x04963e40` for this stream), then signals the condition through
   `0x0403228c`.

No service cache, property result other than the D563 output itself, secure
route, other endpoint, or later display state participates in this return
gate.

## Linux callback/reply ledger

The valid pristine-session reference is:

`neo-bringup/logs/dcp-appledrm-25g83-ep37-early-reply-9670983-20260829-181955.log`

The outer `A401` request is AP local stream 0, raw word `0x1000000202`, shared
stack offset 0, size `0x10`, tag/input/output `0x41343031/0/4`.

Three nested AP-call replies arrive before the final property tail:

| Reply | Raw word | AP channel | Request | Result |
| ---: | ---: | --- | --- | ---: |
| 0 | `0x642` | OOB command stream 1 | A386 | `1` |
| 1 | `0x42` | command stream 0 | A131 | `0` |
| 2 | `0x42` | command stream 0 | A132 | `0` |

The last five firmware callbacks are:

| Callback | Property | Request word | Direction/stream | Output | ACK |
| ---: | --- | ---: | --- | ---: | ---: |
| 177 | `MaxVideoSrcDownscalingWidth` | `0x5c00000002` | DCP to AP, stream 0 | `1` | `0x42` |
| 178 | `VideoClock` | `0x5c00000002` | DCP to AP, stream 0 | `1` | `0x42` |
| 179 | `PixelClock` | `0x5c00000002` | DCP to AP, stream 0 | `1` | `0x42` |
| 180 | `DisplayClock` | `0x5c00000002` | DCP to AP, stream 0 | `1` | `0x42` |
| 181 | `DispPerfState` | `0x5c00000002` | DCP to AP, stream 0 | `1` | `0x42` |

All five have input/output sizes 76/4, shared callback base `0x60000`, offset
0, total size `0x5c`, and mailbox send result 0. The wire words contain no
request ID or sequence field. Their only host-visible difference is callback
order and the property string in shared memory. The first four ACKs are
causally proven to associate and wake because firmware emits the next D563
only after the synchronous caller returns. The fifth has no successor.

At timeout Linux remains at callback count 182, nested reply count 3, outer
command depth 1, stack top `0x10`, and outer request `0x1000000202`.

## Failed AP memory observer

Two diagnostic runs placed observation cookies in bits 48 through 63 of the
five D563 acknowledgements and tried to read the firmware stream response from
the AP:

- `dcp-appledrm-25g83-d563-fw-assoc-9a78abd-20260829-194417.log`
- `dcp-appledrm-25g83-d563-fw-assoc-both-2a08232-20260829-194747.log`

Both reads returned zero. That result cannot describe reply association:
callbacks 177 through 180 are positive controls, because each next D563 can be
sent only after the preceding synchronous wait returns. Their cookies were
absent too.

The observer incorrectly treated root-task VM address `0x04963d80` as a DART
IOVA. Translating IOVA `0x04960000` produced physical address
`0x101edf78000`, outside the loader-published `dcp_data` range
`0x101ea7d4000..0x101eb48ffff`. The executable layout gives the real backing:
the link object is at bundle offset `0x009f1d80`; `dcp_data` starts at bundle
offset `0x004e0000`; therefore the object is at `dcp_data + 0x511d80`, and its
local-stream-0 response is at `dcp_data + 0x511e40`. In that boot this would
have been physical address `0x101eace5e40`.

Even that physical address is not a sound AP observer. The store at
`0x041b781c` targets DCP-private write-back memory and the ingress path performs
no data-cache clean to RAM before signaling its DCP-local condition. An AP
mapping can therefore retain the old RAM value while the DCP CPU consumes the
new cached value. Static bytes elsewhere in `dcp_data` can attest the physical
mapping, but they cannot positively control visibility of this dirty runtime
cache line.

Bits 48 through 63 remain ignored by the dispatcher and synchronous caller,
but there is no proven coherent path by which the AP can recover them. Bits 16
through 47 cannot be used as an injected discriminator: `0x041b552c` reads
that field and `0x041b5530` branches on nonzero to `0x041b56e0`, which reaches
the firmware abort path at `0x04021f18`. It does not return a diagnostic error
to the D563 or A401 caller.

The invalid driver observer and its cookies were removed. Mailbox send result
zero still proves only FIFO admission. There is currently no safe,
positive-controlled AP observation that distinguishes a fifth reply which was
not associated from one whose DCP-local condition did not wake. No further
hardware candidate follows from this memory method.

## Safe A2I transport discriminator

The reply ingress itself has no silent store-without-signal branch. The reply
filter must return an existing busy stream. A missing stream, a newly allocated
stream, or a failed gate action reaches the non-returning firmware abort at
`0x041b7520..0x041b7528`. On the normal branch, `0x041b7450` enters the stream
gate. Action `0x041b7804` stores the raw reply at `0x041b781c` and immediately,
unconditionally calls the condition signal at `0x041b7820`.

That makes the ASC A2I FIFO a useful safe boundary even though it cannot expose
the DCP-private stream word. On T8140 its control register is at mailbox offset
`0x110`. It reports FIFO occupancy in bits 23 through 20, read and write
pointers in bits 15 through 12 and 11 through 8, and empty in bit 17. Reading
the control register is status-only; only the receive-data registers pop FIFO
entries.

Linux handles application endpoint 37 and crashlog endpoint 1 on the same
ordered RTKit workqueue. The diagnostic therefore samples as follows without
altering a reply word:

1. Read A2I control immediately before and after each of the five D563 ACKs.
2. At the next D563 callback, use the same pointer transition to recover the
   previous ACK as a positive control. The callback itself independently proves
   that its predecessor associated and woke the synchronous firmware caller.
3. Read A2I control at the EP1 buffer request before sending the EP1 ACK. This
   work item runs after callback 181 and before EP1 adds another A2I entry, so it
   is an unambiguous observation of ACK5's transport state.

For example, empty state `0x27701`, queued state `0x107801`, and consumed state
`0x28801` describe one send: write pointer 7 to 8, then read pointer 7 to 8 and
occupancy 1 to 0. Strict FIFO ordering means unrelated later traffic cannot
produce a false consumed result. It can only leave the FIFO nonempty and make a
sample inconclusive. The driver accepts only an initially empty FIFO and an
exact one-entry pointer transition.

This classifies transport, not the private stream cache line. If ACK5 is still
the one pending entry at the EP1 sample, it did not leave the AP-to-DCP FIFO. If
it has been consumed, the remaining fault is downstream of hardware delivery,
between RTKit EP37 dispatch and resumption of the synchronous caller. Because
the store and signal are unconditional once the gate action runs, there is no
evidence-derived wire change to make at that point.

## Hardware FIFO result

Fresh-session log:

`neo-bringup/logs/dcp-appledrm-25g83-d563-a2i-18c7a9e-20260829-200723.log`

Callbacks 178 through 181 each recovered their predecessor with
`previous_consumed 1 causally_associated 1`. The fifth acknowledgement then
showed:

```
ack5_before 0x2ff01 ack5_after 0x10f001 ep1_before_ack 0x20001
ack5_consumed 1 ack5_pending 0
```

`0x2ff01 -> 0x10f001 -> 0x20001` is an exact empty, one-entry queued,
empty transition with read and write pointers both advancing from 15 to 0.
ACK5 therefore left the physical FIFO before the EP1 reply was sent. At the
three-second A401 timeout, control `0x100101` described exactly one pending
entry with pointers 0 and 1. The only later AP send was the EP1 buffer reply,
so that EP1 reply remained queued while DCP stopped draining A2I.

The lost-wake theory does not provide a fix. The stream condition wait at
`0x04032108` asserts the event in the current thread before releasing its gate
and blocking. Reply ingress enters the same stream gate, stores the response at
`0x041b781c`, and then signals. A normal reply cannot signal before the waiter
is armed.

## EP37 software scheduling boundary

All five raw `0x42` replies take the same firmware path. The endpoint-37 filter
at `0x041b6fd4` has no property index, callback count, payload, or fifth-call
branch. It signals the same link event source; the AFK dispatcher then enqueues
the word on the same endpoint software queue; action `0x041b70d0` drains that
queue and chooses the same stream from the same control bits. There is no
static ACK1-through-ACK4 versus ACK5 dispatch delta to patch.

Firmware creates the default endpoint workloop named `rtk_ep_work` at
`0x0402125c` with priority `0x39`. It creates the DCPLink consumer workloop
named `iomfb_ap_link` at `0x042df470` with priority `0x10`. The scheduler
records a runnable priority in a 64-bit bitmap and selects its highest set bit
with `clz` at `0x040205c4..0x040205e0`. Those facts do not by themselves close
the signal-before-enqueue question. The route callback at `0x04026bf0` runs
through the lower MBI provider's registered callback, and its exact execution
context is not pinned to `rtk_ep_work` by the current disassembly.

The reply lookup under the link gate at `0x041b7830` aborts if it must create
an idle stream for a reply. A normal synchronous callback cannot expose that
state: `0x041b5138` marks the stream busy before transmit, waits for the reply,
and only its later cleanup action `0x041b50f4` clears the busy word at stream
offset `0x60`. There is no normal branch that clears it while the caller is
blocked. For an existing stream, `0x041b7804` stores the reply and signals the
waiter unconditionally.

Together with the FIFO result, this moves the physical boundary past A2I
dequeue but not past EP37 software dispatch. A missing or newly allocated
stream takes the non-returning abort at `0x041b7520`; it cannot look like a
silent wait. If that abort happened, the immediately following crashlog buffer
request would be consistent with the root task entering its crash path. The
remaining static classifications are route dispatch, that abort, and a later
abort in the five object releases at `0x042ac100..0x042ac19c`.

## Adopted endpoint bookkeeping result

Linux does have a concrete producer-side state mismatch before those replies.
For an adopted session, `dcp_start_rtkit_endpoint()` sends the inherited raw
STARTEP message but did not set the corresponding bit in
`apple_rtkit.endpoints`. Non-adopted `apple_rtkit_start_ep()` requires that bit,
and native RTBuddy records an endpoint when it creates/enables it. The valid
18c7 run consequently emitted ten synchronous `dev_warn` lines for EP20 and
186 for EP37, including one before every A401 callback and nested reply.

This is not only cosmetic. In 18c7, A401 began at `25.922042` and ACK5 was sent
at `26.029092`, 107.05 ms later. The firmware's EP1 request was serviced at
`26.029216`, immediately at that tail. Across fresh runs it appears about
99--107 ms after A401. Each undiscovered-EP37 warning precedes callback
dispatch by about 0.11 ms in the same ordered RTKit worker, accounting for
roughly 20 ms of avoidable serialized latency. The endpoint bit is now recorded
immediately after a successful adopted STARTEP. This preserves all wire words
and makes Linux's software endpoint ownership match the STARTEP it already
sent.

Fresh-session log:

`neo-bringup/logs/dcp-appledrm-25g83-adopted-ep-544562c-20260829-202553.log`

Both endpoint markers appeared and no undiscovered-endpoint warning remained.
A401 still timed out after 182 callbacks. Its fifth D563 acknowledgement was
consumed, and EP1 GETBUF followed immediately:

```
A401 TX                 47.702068
callback 181 ACK        47.789584
EP1 GETBUF              47.789699
ack5 before/after       0x20001 -> 0x100101
EP1 sample              0x21101
timeout                 0x101201
```

Removing the warnings shortened the A401 callback sequence from about 107 ms
to 87.516 ms, but EP1 still followed ACK5 by 115 microseconds. This disproves
both the warning perturbation and an approximately 100 ms timer as the cause.
The endpoint bookkeeping fix remains correct.

## Exact EP1 request classification and disproven early dispatch

Request `0x12610008784000` has bit 54 clear, but that bit alone does not make
it a buffer request. Native `RTBuddyCrashlogEndpoint::_messageHandler` first
checks whether endpoint offset `0xa8` already holds a crashlog buffer. On a
live inherited session it does. Because the word also has a nonzero DVA, the
branch at native kernelcache address `0xfffffe000b1fc158` invalidates that
existing buffer, records crash state 2 at endpoint offset `0x124`, and
tail-calls `handleCrashlog(false)` at `0xfffffe000b1fc1b0`. It sends no reply.

Linux starts with an empty local `crashlog_buffer`, so the generic handler
instead enters the fresh-buffer branch at `0xfffffe000b1fc2f0`. If that branch
is used, the response constructed at `0xfffffe000b1fc380..0xfffffe000b1fc3c0`
contains the buffer DVA in bits 43 through 0, the size in bits 51 through 44,
and the selected page-size bit. It does not set bit 54.

The bit-54, no-DVA response at
`0xfffffe000b1fc298..0xfffffe000b1fc2e0` belongs only to an incoming request
whose bit 54 is set. Linux previously sent `0x52600000000000`, which takes
that other form. The retained worker-path correction now includes the DVA,
uses the native size selector, and leaves bit 54 clear. For the observed
0x26000-byte request and its DVA, the exact response is the original word
`0x12610008784000`.

The missing inherited endpoint object cannot explain ACK5's already-proven
physical dequeue. It does combine with a host scheduling mismatch: native
RTBuddy dispatches the crashlog endpoint independently of DCPLink, while Linux
queues EP1 and EP37 on one ordered RTKit worker. Linux's built-in RTKit core
only offers its atomic early callback to application endpoints at or above
`0x20`.

Commit `e0669808e8968` tested the scheduling difference without modifying
DCPLink traffic. It wrapped only the adopted DCP mailbox receive callback,
intercepted the first matching EP1 word, and echoed the exact raw request from
atomic receive context.

Fresh-session log:

`neo-bringup/logs/dcp-appledrm-25g83-ep1-early-e066980-20260829-205545.log`

The exact result was:

```
EP1 raw RX/echo     0x12610008784000
RX timestamp        33376211990 ns
A2I before/after    0x20001 -> 0x100101
send result         0
A2I at timeout      0x100101
classification      consumed 0, pending 1
```

A401 still timed out at callback 182 with three nested replies. The response
was admitted immediately but remained the sole unread A2I entry. This
eliminates the fresh-buffer reply payload and Linux's shared EP1/EP37 worker
as causes. DCP had already stopped draining A2I after consuming ACK5. The
mailbox-wrapper diagnostic was removed. The fallback fresh-buffer response is
now wire-correct, but native's inherited-buffer path establishes that this
specific word is a crash notification and needs no response.

The single pending A2I word at timeout in the e066 run is therefore a Linux
artifact, not firmware backpressure. Linux lost endpoint offset `0xa8` while
adopting the live RTBuddy session, misclassified the crash notification as a
fresh buffer request, and queued a response native never sends. This happens
after the firmware has already entered its crash path and is not the A401
cause. The queued parity fix is to import or retain the inherited EP1 buffer
object before endpoint traffic, invalidate/decode its current crash record on
this notification, and send no reply. Keep that state repair separate from the
current crash-PC discriminator.

## Timeout crashlog discriminator

The adopted path does not send a new STARTEP for EP1. It marks the system
endpoint as inherited from the previous owner and starts only the mailbox.
The fixed firmware DVA in the EP1 word therefore names the previous owner's
already-established crashlog buffer. Native's message-handler branch proves
that its arrival immediately after ACK5 is a crash notification, not a buffer
request. The first Linux worker read can occur while the record is still being
published and has not found a `CLHE` header in retained runs.

The timeout diagnostic rereads that existing Normal-NC mapping only after the
three-second A401 timeout. It sends no mailbox word and changes no callback or
wait timing. The first run exposed the multi-generation header signature
`0x434c4748` (CLGH) rather than the older direct CLHE layout.

The native selector is
`RTBuddyCrashlogDecoder::_multiCrashlogFindFirstCrash` at
`0xfffffe000b20553c`. Its exact CLGH layout is:

| Offset | Size | Meaning |
| ---: | ---: | --- |
| `0x00` | 4 | signature `0x434c4748` |
| `0x04` | 4 | version; byte 1 must equal 2 |
| `0x08` | 4 | total buffer size |
| `0x0c` | 4 | current generation ID |
| `0x10` | 4 | record count |
| `0x14` | `count * 0x18` | generation records |

Each record begins with a 64-bit generation ID and contains its 32-bit payload
offset at `+0x10`. Native chooses the record whose ID equals the header's
current ID. If that payload also begins with CLGH, it repeats the same lookup;
otherwise `decodeLog` requires CLHE signature `0x434c4845` and version byte 1
equal to 3. This makes the selected Crg8 and Cstr sections current-generation
evidence rather than a scan which could find a stale record.

Native rejects a selected CLHE while flags bit 8 is set. It also requires the
final 32 bytes at `selected + total_size - 0x20` to exactly match the initial
CLHE header and constrains all sections to the range between those headers.
For Crg8, payload word `+0x4` must equal `0x348`; payload word `+0x0` bit 16
marks the register record non-decodable. Only after those gates is PC read at
payload `+0x108` (section `+0x118`). The timeout observer and offline parser
both enforce these conditions.

The observed recovery-boot bytes
`48474c43020200000060020055344c430200000055344c430000000000000000`
decode as size `0x26000`, current ID `0x434c3455`, count 2, and record-zero ID
`0x434c3455`. They stop before record zero's payload offset at outer offset
`0x24`, so that non-comparable run cannot identify a PC.

The corrected timeout observer follows the native current-generation chain,
then prints Crg8 PC/SP/PSR/FAR/ESR and Cstr messages. A valid pristine A401 run
with `DCP25G83_A401_CRASHLOG_CRG8` will distinguish the EP37 invalid-stream
abort from an object-release abort by its exact firmware PC and crash string.

## Queued post-A401 H17P callback table correction

This is downstream ABI evidence and must not be folded into the crash-PC
diagnostic. The exact native H17P A4xx table from A442 onward is an older
revision than Linux currently models:

| Native tag | Contract | Current Linux tag |
| --- | --- | --- |
| A449 | set_parameter, input `0x28`, output `0x4` | A442 |
| A453 | create_default_frame_buffer, input `0`, output `0x4` | A446 |
| A457 | video power savings, input `0x4`, output `0x4` | A450 |
| A465 | first_client_open, input `0`, output `0` | A457 |
| A466 | last_client_close, input `0x4`, output `0x4` | A458 |
| A474 | display refresh properties, input `0`, output `0x4` | A465 |
| A477 | supports_power, input `0x4`, output `0` | A468 |
| A478 | abort_swaps, input `0x8`, output `0x4` | A469 |
| A483 | power_state, input `0x10`, output `0x8` | A473 |

Native also inserts A446 `check_task_status` (`0x0c`/`0x4`) and A468
`flush_debug_flags` (`0x4`/`0`). The corrected D120 nested sequence is A389,
A453, A024, A477 returning true, A000, then A474. Apply and test this table as
a separate post-A401 change only.

The same downstream correction moves bandwidth allocation back to D129 and
adds the native tail calls which current H17P lacks:

- D130 `update_ext_pipe_count(bool, bool, u32)`: input `0x08`, output `0`;
  bools at `+0`/`+1`, unaligned u32 at `+2`, and two padding bytes.
- D131 `get_ext_pipe_count(u32 *, u32 *, u32 *)`: input `0x10`, output `0x0c`;
  three u32 values at `+0`/`+4`/`+8`, null bytes at `+0c`/`+0d`/`+0e`, and
  padding at `+0f`; output is three u32 values.

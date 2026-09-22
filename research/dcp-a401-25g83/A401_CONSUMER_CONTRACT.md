# 25G83 DCPLink A401 consumer contract

## Result

The T8140 PMC prerequisite removes the original fifth-`D563` fabric abort.
Firmware now crosses `DispPerfState` and emits another sixteen callbacks. The
last visible pair is `D102 PanelLayout`, then `D102 SupportRelbufInfoCb`.
Linux sends the final raw `0x42`, the ASC A2I FIFO is empty at timeout, but
firmware emits neither the immediately following `D000` nor `D001` and never
returns the outer `A401` reply. The exact endpoint dispatcher reveals a quiet
signal-before-publish race: the EP37 filter signals `iomfb_ap_link` before the
dispatcher publishes the raw acknowledgement into EP37's software queue, and
that later queue push does not signal the workloop.

The native AP-side `D102` implementation creates an OSNumber and publishes it
through `IORegistryEntry::setProperty`; it sends no nested DCP message and its
return value is not represented on the wire. The exact firmware continuation
has no PMC/PMP wait and no callback-table dispatch between the final `D102`
and `D000`. The current proven boundary is therefore after physical dequeue
of the final acknowledgement but before the synchronous DCPLink sender has
resumed far enough to issue `D000`. The single-variable discriminator sends a
normal type-1 READY word immediately after the exact final
`SupportRelbufInfoCb` acknowledgement. FIFO ordering puts it after the real
reply; its second EP37 filter invocation re-signals the workloop only after the
reply has been published. It changes no RPC bytes and adds no delay.

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

All raw `0x42` replies take the same firmware path. The endpoint-37 filter at
`0x041b6fd4` has no property index, callback count, payload, or final-call
branch. The difference is a scheduler race around the common path, not a wire
decode delta.

The generic incoming dispatcher at `0x04026bf0` calls the endpoint route/filter
at `0x04026cc8`. For an EP37 reply, the filter triggers the link event source at
`0x041b705c` and returns zero. Only after that return does the dispatcher push
the raw word into the endpoint software queue at `0x04026cf8`. The queue push
does not trigger the event source. Therefore the ordering is exactly:

1. mark the link event source pending and wake its workloop;
2. return from the EP37 filter;
3. publish the raw reply to the EP37 software queue.

The workloop action loop confirms the quiet window. It tests the pending byte
at `0x04031c6c`, clears it before invoking the action at `0x04031c7c`, and calls
the action at `0x04031c94`. Action `0x041b70d0` returns zero when its queue is
empty. Only a nonzero action result explicitly rearms the source at
`0x04031ccc..0x04031cd0`; a concurrent trigger can also set the byte again, but
the later queue push cannot. If `iomfb_ap_link` runs in the interval between
steps 1 and 3, it observes an empty queue and consumes the only notification;
the raw reply then remains queued indefinitely.

Firmware creates the default endpoint workloop named `rtk_ep_work` at
`0x0402125c` with priority `0x39`. It creates the DCPLink consumer workloop
named `iomfb_ap_link` at `0x042df470` with priority `0x10`. The scheduler
records a runnable priority in a 64-bit bitmap and selects its highest set bit
with `clz` at `0x040205c4..0x040205e0`. The normal priority relationship
usually keeps the incoming dispatcher running until publication, which
explains the positive controls. It is not an ordering primitive: the signal
and queue push are separate operations with no post-publish notification.

The reply lookup under the link gate at `0x041b7830` aborts if it must create
an idle stream for a reply. A normal synchronous callback cannot expose that
state: `0x041b5138` marks the stream busy before transmit, waits for the reply,
and only its later cleanup action `0x041b50f4` clears the busy word at stream
offset `0x60`. There is no normal branch that clears it while the caller is
blocked. For an existing stream, `0x041b7804` stores the reply and signals the
waiter unconditionally.

Callback 196 and callback 197 have indistinguishable wire, stream, route, gate,
and host-handler contracts. Callback 196's acknowledgement resumes firmware
and produces callback 197 only 131 microseconds later. Neither D102 tail
releases the outer stream, detaches the event source, nor changes workloop
ownership. Callback 197 is special only because no later firmware callback
provides an incidental second signal if its acknowledgement loses this race.

The delay-free discriminator uses raw type-1 READY (`0x1`) as that second
normal notification. It is sent through the same ordered A2I mailbox primitive
immediately after the exact final D102 acknowledgement. When firmware dequeues
READY, its EP37 filter signals the same event source after the acknowledgement
has already been published. Type 1 follows the normal branch at `0x041b71c4`
to `0x041b7670`; the DCP returns READY for its normal local-side state, and
Linux ignores READY once the link is running. A successful discriminator
reaches the following `D000` and `D001`; an unchanged timeout falsifies this
race as the remaining liveness cause.

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

The parity path now imports exact 25G83 word `0x12610008784000` before DCP
endpoint traffic. It seeds size `0x26000`, DVA `0x10008784000`, validates the
entire DART range, and maps it Normal-NC. Generic RTKit consequently enters its
existing-buffer crash path on EP1 and sends no reply. This repairs post-crash
host state only; it does not change the pre-crash fifth-D563 path.

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

Fresh main-path result:

`neo-bringup/logs/dcp-appledrm-25g83-clgh-current-c4a1258-20260829-212020.log`

The fifth D563 acknowledgement was sent at `25.692938`; EP1 arrived at
`25.693057`. The current record decoded as generation `0x434c344b`, depth 1,
offset `0x21000`, CLHE version `0x302`, size `0x4e4`, flags `0x1f`, and Cstr
`exception.c:59`. Its sections were Ctim `0x18`, Crl4 `0x378`, and CasC
`0x48`. The inherited-buffer handler marked DCP crashed; Linux's old fallback
reply was rejected with `-EINVAL` only after that. This confirms a real crash
and rules out any EP1 request/reply handshake as its cause.

The 25G83 firmware template at decompressed bundle file offset `0xeb60` gives
Crl4 signature `0x43726c34`, version `0x401`, and section size `0x378`. Crl4
is not the Crg8/ARMv8 `0x348` frame. Its exact 25G83 cL4 section-relative
layout has flags at `+0x10` (bit 16 marks it non-decodable), nominal size
`0xd8` at `+0x14`, and raw context at `+0x18`. The decoded fields are LR
`+0x108`, PC/ELR `+0x110`, SP `+0x118`, PSR `+0x120`, FPSR `+0x128`, FPCR
`+0x130`, TPIDR_EL0 `+0x138`, ESR `+0x358`, and FAR `+0x360`.

The observer and offline parser keep the Crg8 and Crl4 gates distinct.
`exception.c:59` identifies the firmware kernel's generic exception recording
site; it does not identify the faulting instruction. The next valid
discriminator is `DCP25G83_A401_CRASHLOG_CRL4 ... pc ...`, which can locate
the actual pre-notification failure against the exact 25G83 image.

The corrected main-path run with commit `00f9edd` decoded LR `0x042a13d0`,
PC `0x04141324`, PSR `0x60400000`, ESR `0x92000410`, and FAR
`0x3028aa140`. ESR EC `0x24` is a lower-EL data abort, DFSC `0x10` is a
synchronous external abort, WnR is clear, and FnV is set. FAR is therefore not
valid. The exact instruction at PC is `pacibsp`, followed by the stack-frame
prologue; it performs no data access. The saved LR points at `mov w6, w19`
inside the UPBlock/PTUC register-programming family, not at a call instruction.
Neither address can be treated as a precise faulting read.

Strings and control flow identify the PC function as
`getRequestedDriveSettings`. Its first external operation is an AUX/DPCD read
of adjustment-request register `0x206`, with an optional second read of `0x335`.
That identity locates the task active when the fabric abort was reported; it
does not prove that the AUX read caused the abort. Patching or delaying this
function would contradict the Crl4 syndrome.

The remaining current-generation section is CasC version `0x101`, total size
`0x48`. Its native payload begins at section `+0x10` with seven 64-bit fabric records:
L2C error status, L2C error address, L2C error info, LSU error status, FED error
status, MMU error status, and a 25G83-only extra qword which older decoders
ignore. Commit after `00f9edd` logs the exact 56-byte payload, all seven qwords, and
the Crl4 x0--x29 set without changing mailbox, RPC, or hardware behavior. This
is the first discriminator that can name the async source/master and distinguish
a missing PTUC mapping from a powered-off display block. No behavior change is
justified until those values are known.

## PMC prerequisite result

Fresh CasC run:

`neo-bringup/logs/dcp-appledrm-25g83-casc-async-87dda47-20260829-215244.log`

The seven qwords were:

```
l2c_err_sts = 0x0000000000000080
l2c_err_adr = 0x03000003028aa140
l2c_err_inf = 0x0000001000000003
lsu_err_sts = 0x23800003028aa140
fed_err_sts = 0
mmu_err_sts = 0
extra       = 0
```

The exact producer is the continuation of callback 181, `D563 DispPerfState`.
`RTKPMGR_pmc_boost_disp_perf_state` reaches leaf `0x0403e318`, which uses
static display-PMC base `0x302800000` and reads word offset `0xaa140`. This is
physical address `0x3028aa140`, exactly the CasC address. Zero MMU and FED
status rule out the display DART and the earlier PTUC hypothesis. Linux had
left the T8140 `ps_pmc` domain off while the inherited firmware still treated
the PMC as available.

Commit `9e9a1a4` applies only the native PMGR power-on sequence to the exact
T8140 PMC descriptor before EP20/EP37 startup. The PMP report registers remain
read-only. Valid fresh-session log:

`neo-bringup/logs/dcp-appledrm-25g83-pmc-prereq-9e9a1a4-20260829-224514.log`

```
DCP25G83_PMC_PREREQUISITE before 0x0f000200
    target_write 0x1f00000f after 0x1f0002ff
    target 0x0f actual 0x0f auto 1 result 0
```

There is no CasC record or crash notification in this run. Callback count
advances from 181 to 198, proving the PMC change fixes the external abort.
PMP report status remains zero and its DISP target remains requested with
actual zero, but that state does not stop the firmware at the old PMC read.

## Post-PMC callback tail

The new callbacks are chronological and synchronous:

| Sequence | Tag | Contract / observed key | Linux output |
| ---: | --- | --- | --- |
| 182 | D125 | `initial-vbi-advance-lines`, `0x64`/`0x24` | first value `2` |
| 183 | D561 | `M3TimingParameters` | true |
| 184 | D552 | `IOMFBNumLayers` | true |
| 185 | D552 | `IOMFBMaxSrcPixels` | true |
| 186 | D101 | default stride | zero |
| 187 | D552 | `DisplayPipePlaneBaseAlignment` | true |
| 188 | D552 | `IOMFBScalingLimits` | true |
| 189--192 | D565 | Y/X flip support, twice | true |
| 193--195 | D565 | ALSS/ACSS/SPLC support | true |
| 196 | D102 | `PanelLayout`, `0x44`/0 | no output |
| 197 | D102 | `SupportRelbufInfoCb`, `0x44`/0 | no output |

The observed D125 request uses the read-EDT-data ABI and firmware continues
through fifteen later callbacks. It therefore cannot be the immediate final
gate even though the separately recovered newer host table labels that ABI
D124. Do not renumber this live path until the exact host/firmware revision
pair is reconciled.

Native `UnifiedPipeline_RemoteCalls::D102_callback__` validates `0x44` input
bytes, finds the IOMobileFramebufferAP instance, and calls
`UnifiedPipeline2::set_number_property`. That routine allocates an OSNumber,
calls the IORegistry `setProperty` vtable slot, then releases it. D102 has no
output and this path sends no nested DCP message. Omitting the registry side
effect is a host-parity issue, but it cannot keep the firmware's synchronous
sender waiting after the outer `0x42` arrives.

## Exact continuation after the final D102

Both final property calls are made by `0x042ae47c` through the same D102 thunk
at `0x04268284`. `PanelLayout` calls it at `0x042ae5d8`.
`SupportRelbufInfoCb` calls it at `0x042ae5fc` for value one or
`0x042ae624` for value zero. After that call returns, the function only emits
a log and returns to `0x042b5838`.

The remaining instructions before the next callback contain no hidden power
or property wait:

1. `0x042b583c` calls `0x042ae330`. Its comparison uses the same string pointer
   for both arguments, so the zero result takes the immediate return branch.
2. If the optional object at UPPipe offset `0x2c18` exists, slot `0x138`
   reaches `0x042835cc -> 0x04238e80`; the final UPPipe slot `0x110` target is
   the bare `ret` at `0x0423c494`.
3. Optional-object slot `0x140` reaches
   `0x0428317c -> 0x04238e18 -> 0x042a8b40`. Its last virtual target is
   `0x0423a8ec`, which returns zero; the chain only derives a boolean.
4. `0x042b58b4` synchronously sends D000. `0x042b58cc` synchronously sends
   D001.

Neither D000 nor D001 appears in the valid log. Firmware therefore has not
returned from the final D102 synchronous call. This eliminates the D102
registry side effect, later callback-table entries, and PMC/PMP policy from
the immediate continuation.

## Callback 196/197 reply association boundary

Callbacks 196 and 197 are an exact transport control pair. Each has raw request
word `0x5000000002`, stream selector zero, side selector zero, shared RPC base
`0x60000`, offset zero, total size `0x50`, input `0x44`, output zero, and raw
acknowledgement `0x42`. The acknowledgement contains no callback tag, key, or
sequence field. Firmware can emit callback 197 only after callback 196's
synchronous D102 sender resumes, so callback 196 is a positive association and
wake control for the same wire word and stream object.

For either acknowledgement, EP37 action `0x041b70d0` takes the reply branch at
`0x041b73ec`. Selector equality in `0x041b7830` chooses local stream zero,
object `0x04963df8`. The existing busy-stream branch is mandatory. A missing
stream, a newly allocated stream, or reply lookup/gate setup failure reaches
the non-returning abort at `0x041b7520..0x041b7528`.

The hardware receive callback reaches `0x04026bf0`. It records the lower
mailbox entry at `0x04026c7c`, resolves the endpoint, and invokes the EP37
filter at `0x04026cc8`. The filter triggers the event source before the
endpoint queue push at `0x04026cf8`. Triggering is latching with respect to
action execution, but not with respect to the later producer: the workloop
clears pending before the action, and an empty action returns zero. Consumer
action `0x041b70d0` pops EP37 words at `0x041b710c` and drains the queue only
if publication wins the race.

For an existing stream, `0x041b76e0` asks the stream gate at offset `0x58` to
run action `0x041b7804`. The generic gate helper at `0x04032058`
unconditionally calls the supplied action at `0x0403207c` and returns that
action's result at `0x0403208c`. Action `0x041b7804` always returns zero. The
defensive nonzero test at `0x041b7454` is therefore unreachable for this
action; it is not a third silent failure class. If reply dispatch reaches the
existing-stream gate, it necessarily stores the raw response at offset `0x48`
and immediately signals the stream event.

The synchronous sender sleeps through the same stream gate at `0x041b542c`,
checks the stored low 16 bits at `0x041b5438..0x041b5444`, and accepts `0x42`.
Its stream workloop was created at priority `0x10`; its pointer is installed at
stream offset `0x58`, and its owning thread identity at offset `0x38`.
Reply dispatch runs on the distinct `iomfb_ap_link` workloop, also priority
`0x10`. The sender's command sleep releases the stream gate before blocking,
so the reply action can enter it. Only later cleanup at `0x041b50f4` clears the
stream busy word.

At timeout the A2I control value is `0x20001`: empty, with equal read/write
pointers. No AP message follows callback 197 in the fixed inherited-crashlog
path. Thus its acknowledgement left the physical FIFO. This does not prove
that the software endpoint ran the existing-stream gate action. The stream
response and busy fields are DCP-private write-back cache lines; the failed
cookie observer already proves AP reads of their RAM backing are not coherent.
The shared RPC buffer contains no response field for an output-less D102.

The ordered READY notification is a positive discriminator for this exact
producer ordering bug. It does not alter the acknowledgement, inject an error,
double-send a reply, or depend on an incoherent DCP-private cache line.

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

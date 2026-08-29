# 25G83 DCPLink A401 consumer contract

## Result

The observed `A401` hang is inside the firmware's fifth synchronous `D563`
callback. It is not in a later display property, service, side endpoint,
`start_hardware_boot`, or post-start state gate.

The fifth callback is `DispPerfState`. Linux writes output `1` and queues the
same raw EP37 acknowledgement form as the four callbacks immediately before
it. The firmware never sends `D121` (`start_hardware_boot`) and never returns
the outer `A401` reply. Static control flow proves that the only gate between
the fifth `D563` call and its return is the DCPLink stream response word and
condition wake.

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
`0x042654b8` and sends `D121` at `0x042654f4`. The observed log has no `D121`,
which proves execution has not crossed the fifth `D563` call site.

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
   each stream object is `0x68` bytes. The callback stream used here is the
   remote stream 0 object at `0x04963f98`.
4. `0x041b76e0` enters that stream's gate.
5. Action `0x041b7804` stores the full raw acknowledgement at stream offset
   `0x48` (`0x04963fe0` for this stream), then signals the condition through
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

## Current discriminator

The unresolved split is now only:

1. the fifth host ACK never reaches or associates with firmware remote stream
   0; or
2. ingress stores it in the stream response word, but the condition signal or
   waiter wake does not complete.

Mailbox send result 0 proves admission to the AP-to-DCP FIFO, not firmware
association. The driver diagnostic built from this contract uses an ignored
high-16-bit observation cookie on only the five D563 ACKs. Static firmware
control flow proves reply dispatch uses only the low 16 bits, the caller masks
the response with `0xc3`, and the remote error read covers bits 16 through 47.
The cookie therefore does not alter reply classification, stream selection,
success, or error behavior. At timeout the AP reads the exact firmware stream
response location `0x04963fe0` through its existing DART mapping. A fifth
cookie there proves the firmware associated the fifth ACK; an earlier cookie
proves it did not.

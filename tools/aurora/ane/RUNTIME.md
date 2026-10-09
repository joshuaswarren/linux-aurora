# Private ANE application runtime

The optional stories260K build exposes `/dev/aurora-ane0` for applications to
submit FP16 tensors to the validated ANEMLL program. The original sysfs
qualification tests remain available in their builds. This interface is
private and experimental; it is not an upstream Linux UAPI.

The runtime passed 256 jobs and 768 ordered events on real Neo Linux,
including repeated ring wrap, rejected requests, recoverable copyback faults,
two eight-token prompts and explicit shutdown. It compared 3,915,776 FP16
values exactly with native ANE outputs and 262,144 with independent HF FP32
outputs; all 16 generated tokens matched. A separate fresh Linux boot also
passed one raw-tensor job and automatic shutdown on final file close, with
16,384 exact native values. Host and ARM64 checks also pass. Evidence is
under `neo-linux/artifacts/j700-ane-runtime-20260913/`; `RESULTS.md` explains
the scope and `hardware-result.json` records the machine-checked receipts.
`verify-runtime-result.py` checks the saved logs, artifact hashes, event
order, prompt tokens, kernel diagnostics and resident verification. Build
and CPU-reference manifests retain their creation-time untested flags;
the separate hardware receipt records the subsequent qualification.

## Build and use

Build the same pinned model case through `build-fw-test.py --case-dir ...`.
The ordered test overlay creates the powered ANE/DART devices and the misc
device. Loading modules does not start firmware. The first application LOAD
starts the pinned firmware if needed, then loads the program. It accepts
only the compiled-in package SHA-256. Models cannot supply their own firmware
commands, IOVAs or physical addresses through this interface.

Compile the client on Linux using the driver's private headers:

```sh
clang -std=c11 -O2 -Wall -Wextra -Werror \
  -I drivers/soc/apple tools/aurora/ane/ane-client.c -lm -o ane-client
```

One raw tensor inference:

```sh
./ane-client /dev/aurora-ane0 --run input.bin output.bin
```

`input.bin` is exactly 4096 bytes: little-endian FP16 embeddings in
`[1,64,1,32]` order. `output.bin` is 32768 bytes of `[1,512,1,32]` FP16
logits. CPU applications perform tokenization and embedding lookup before
submission, then select a token from the logits and form the next input.
The five transformer layers and output head execute on the ANE. The runtime
has no CPU transformer fallback or expected-output comparison path.

The supplied client has a separate `--qualify FIXTURES` mode for evidence:
permissions/ownership, invalid pointers and requests, copyback faults, native
numerical regression, two user-controlled prompts, repeated ring wrap, the
job limit, shutdown and closed-device state. It is not a throughput benchmark.

## Interface and lifetime

`drivers/soc/apple/apple-ane-user.h` defines ABI version 1. Structures use
fixed-width integers and aligned 64-bit userspace addresses. Reserved/output
fields must be zero on submission. INFO is 88 bytes; RUN is 64 bytes.

| Command | Behavior |
| --- | --- |
| INFO | Cached model, sizes, job limit, completed jobs, event count, error and state; no firmware access. |
| LOAD | Load the single SHA-pinned program once per boot. Argument must be zero. |
| RUN | Copy a finite input into private driver memory, execute synchronously, and copy logits/sequence/timing to userspace. |
| SHUTDOWN | Close the program and request firmware shutdown. Repeating a successful shutdown succeeds without new work. |

The device is mode0600 and requires CAP_SYS_RAWIO at open and on every
ioctl, including an inherited file descriptor. Only one open owner is
allowed. Calls are serialized by the device mutex. Legacy sysfs operations
are rejected while an application owns the device. A single in-flight job
uses a fresh sequence, validates token/phase/status and mapped ownership,
and consumes begin/intermediate/end events. Opaque message handles are not
treated as addresses or global event indices.

No userspace page is exposed to ANE DMA. User input is copied before any
submission; DMA targets are fixed private buffers. Output is poisoned before
every job and must be finite after all completion events. Invalid sizes,
reserved fields, inaccessible input and nonfinite input are rejected before
execution. An output-copy or completion-copy fault can occur after execution:
the job remains counted in INFO and the session remains usable. Callers must
not automatically retry a failed ioctl without accounting for those completed
jobs. Applications should serialize their own requests and recovery decisions.

Each session is capped at 256 jobs. The next request returns EOVERFLOW
without submitting work. A final file close requests shutdown if the program
is still healthy and loaded. A protocol/firmware error moves the session to
a retained fault state and blocks further submissions; it does not attempt
an unproven engine reset or free DMA memory.

## Remaining limits

The model still requires macOS compilation and captured native resources.
There is one fixed model/shape, a 32-position stateless context, one owner and
a bounded session. Reopening a closed device allows INFO but does not restart
firmware. The shutdown reply is not proof of safe resource teardown: DMA,
power and module references remain retained until a Neo-only SoC reset.
Normal unload, suspend/resume, arbitrary models, automatic error recovery,
multi-client scheduling and a Linux compiler remain unfinished.

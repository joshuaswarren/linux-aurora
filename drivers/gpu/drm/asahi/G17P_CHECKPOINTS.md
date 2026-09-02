# G17P Vulkan bring-up checkpoints

Keep this list short and evidence-only. A build is not a hardware checkpoint.

## Verified working

- Vulkan compute: repeated exact-output dispatches complete without GPU faults.
- Render TA: the 32x32 Vulkan path publishes and executes TA; both TA hardware
  timestamps become non-zero.
- Fragment admission: firmware selects QID 1, assigns PDM pipe 0
  (`state=0x0100000d`), and publishes the expected current command/context.
- Fragment descriptor reachability: all four SKSM RCE bindings resolve through
  the submitting VM to the same physical descriptor bytes written by the CPU.

## Current boundary

- The fragment's tag-15/tag-14 records retire and hardware selects QID 1, but
  B1 emits no KTrace code 2. Code 2 is in the bit-4 new-work path: the ISR
  reads the QID bitmap at SGX `+0x10a60`, acknowledges it at `+0x10a68`, and
  only then does `0x23004` allocate the firmware job. Other records guarded by
  the same trace-class bit are present, so this is the earliest proven gap.
- SGX `+0x10408` remains at its `0x100` idle/reset value, no bit-3 completion
  interrupt appears, and the resource callback stays idle because all three
  are downstream of the missing new-work interrupt/job allocation.
- The resource-manager clear/go masks and callback state stay zero throughout
  the live post-doorbell polls. KTrace `0x44/0x45` appears only after recovery
  starts, so it is recovery teardown, not the stalled admission callback.
- B1's later 3D activation routine at `0x162f8` writes `1` to fragment
  descriptor `+0x20f0`. On hardware `+0x20f0` and both 3D timestamps remain
  zero, consistent with the earlier state-1 callback stall.

## Corrected claims

- Fragment has not yet executed. Ring counters reaching `3/3/3`, the job list
  becoming empty after recovery, and `3D completion scratch=...timestamp=1`
  are not proof of execution. The scratch value is staged by the host before
  the doorbell, and recovery retires the software-visible ring state.
- The earlier "fragment engine reaches hardware completion" progress update was
  incorrect and is retracted.

## Eliminated at this boundary

- RCE register values, PTC maximum, descriptor tail fields, shader/VDM content,
  and the four RCE VM mappings cannot prevent entry into `0x162f8`; firmware has
  not reached the point where it reads them.
- Native has no tag-4 command for this same-context TA-to-3D dependency.
- Native HardwareBufferID values are allocator results, not fixed ABI values.
- A Linux-only render power request is not the cause. With the power-wire mode
  disabled Linux matches Metal's message ordering, while the same launch bit-3
  failure remains.

## Grounded parameter work

- Native PTC formula is `max(min_count, min(input / max_concurrent_kicks,
  max_count))`, with Neo limits `0x60..0x200` and priority-2 divisor 3.
- The traced cold 32x32 pool state computes TA `0xa9` and 3D `0xd5`. Linux's
  `0x60` is the native zero/unprepared fallback, not the correct prepared value.
- These are per-submit computed values, not architecture-wide literals. The
  durable kernel model still needs stage-specific inputs/computed maxima.

## Next experiment

- Cold Pool-A record 1 is now the kernel default and was verified on hardware:
  submit-ready 3D `+0x20` changed from Pool-A `+0x000` to `+0x100`, matching
  three native capture corpora. TA still completes, while `+0x20f0` and both
  3D timestamps remain zero, so this real mismatch was not sufficient.
- Sample the bit-4 new-work QID bitmap/ack pair (`+0x10a60/+0x10a68`) alongside
  aggregate IRQ status at the fragment doorbell and after TA completion, then
  compare the SKSM queue interrupt/configuration against Metal. Keep the later
  launch registers, descriptor, SKSM, QoS, completion-ring, recovery, and broad
  dockchannel dumps enabled.

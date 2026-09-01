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

- B1 firmware's 3D activation routine at `0x162f8` writes `1` to fragment
  descriptor `+0x20f0` before consuming any RCE register list.
- On hardware, after TA completes and QID 1 is selected, `+0x20f0` remains zero
  and both 3D timestamps remain zero. The blocker is therefore before the B1
  activation routine, in the fragment scheduler admission state machine.

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
- Enable the firmware KTrace class-0 drain and identify the scheduler callback
  state/guard before the `0x14ed8` call to `0x162f8`. Keep descriptor, SKSM,
  QoS, completion-ring, recovery, and broad dockchannel dumps enabled.

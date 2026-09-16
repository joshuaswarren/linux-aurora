# Separate stencil-plane dependency

Official case `KHR-GL30.texture_repeat_mode.depth24_stencil8_49x23_0_clamp_to_edge` independently reproduced the `exclusive writer` assertion in `agx_batch_submit`. The GPU remained healthy; this was distinct from the earlier compression-selector fault.

`agx_batch_reads` retained both the depth BO and its separate stencil BO, but flushed only the depth plane's pending writer. A plane-by-plane blit could leave a different active batch writing the stencil BO when the combined texture reader was submitted. The successful-submit ownership assertion then found that other active writer.

The change resolves the separate stencil writer using the existing `agx_flush_writer_except` helper. It preserves the fake-resource exemption and the original ownership assertion. There is no assertion removal or blanket synchronization.

Before/after hardware evidence uses the same official case, boot `fafdad9c-3a9b-42f7-8264-edbf3ec6dd47`, and unchanged CTS source:

- Before: `hardware-inventory/depth-stencil-isolated`, library `59bd55a15dccb41f0d74f314684c598dae92dc07eee6b04bea5ce6e2eacc9802`, assertion/exit 134, no kernel fault.
- After: `hardware-inventory/depth-stencil-hazard-fix`, library `ef8f21e5b82b3a955d5c72d708490671ab4284c71c652858bf86aa6831c213ec`, complete QPA result, no assertion or kernel fault. The case still reports a depth/shadow rendering failure.

The next unmodified full run completed all 162 texture-repeat cases (126 pass, 36 fail) and all 651 shader cases (pass) in the first main configuration. It later stopped on a GPU memory fault in `KHR-GL30.ext_texture_shadow_lod.texture.sampler2darrayshadow_fragment`. That fault is not yet isolated; its secondary VM errors are consequences, not separate allocation bugs.

Latest selected test counts: 797 pass, 49 fail, three internal errors, one interrupted, 43,051 not run. Fifteen successful configuration checks are counted separately. Full conformance remains incomplete.

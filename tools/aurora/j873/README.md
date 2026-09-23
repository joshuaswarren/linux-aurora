# J873 / T8152 Linux checkpoint

2026-09-23 experimental Mac mini M6 bring-up, branch `j873`.

Hardware verified: Linux boots all twelve CPUs, SMC temperature/fan readings
work, and CPU-frequency cooling-state requests complete at conservative
frequency caps. The Xfce desktop was confirmed visible at 1280x720 through
the inherited USB-C DisplayPort framebuffer.

This branch adds the board DT and 125 sensor definitions, T8152 CPU idle
register preservation, AIC matching, SMC/mailbox support and bounded CPU
frequency command completion. The thermal and desktop directories contain
the separate kernel configurations, RAM-rootfs builders and startup tools.

The frequency driver reports commanded frequency; the independent hardware
status-register layout is not established. The low OPP limits are deliberate.
Temperature readings and exercised cooling requests do not establish full
thermal qualification under sustained high load.

Native DCP DRM, Gen3 DART and G18G GPU execution remain incomplete. The
visible Xfce desktop uses simplefb/software rendering; no accelerated
Gallium GLX result is claimed. USB input is still a bring-up path.

See [desktop setup](desktop/README.md) for the builder requirements and
known-good artifact scope. Firmware, local captures, disk images and machine
credentials are external inputs, not part of this source checkpoint.

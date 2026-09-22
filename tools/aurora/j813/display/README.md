# J813 native DCP display presentation

Hardware validation on September 12, 2026, used the M5 MacBook Air (J813,
T8142) with the 25G76 DCP firmware inherited from the bootloader. The shared
Apple DRM driver completed framebuffer and client initialization, exported
the 2560x1664 panel mode, and ran standard `modetest` pageflips for 20 seconds
at approximately 60 Hz. The user confirmed the visible color-bar pattern.

The same driver then initialized on a second RAM boot with the existing N1
and input root. Xorg selected the Apple DRM device through `modesetting` at
2560x1664; xfce4-session, xfwm4, xfce4-panel and xfdesktop passed the startup
check. N1 obtained DHCP and passed three pings to 8.8.8.8 after its documented
normal-TX-queue activation. Keyboard, trackpad and Bluetooth initialized.
The user confirmed the visible XFCE desktop and smooth trackpad movement.

This establishes the display path. XFCE currently renders in software.
Application GPU submission, hardware-accelerated Mesa and glxgears remain
unfinished. Bluetooth audio and keyboard key events were not retested as
part of this display qualification.

## Native ABI corrections

A successful native macOS hypervisor trace supplied the framebuffer startup
sequence and dimensions. The exact M5 display binary supplied the client
notification method. Shared H17 transport is retained, with explicit H17G
overrides for these verified methods:

| Operation | H17P method | H17G method |
| --- | --- | --- |
| Video limits | A025 | A029 |
| Default framebuffer compression information | A035 | A039 |
| Default framebuffer information | A034 | A038 |
| Default framebuffer query | A033 | A037 |
| Client notifications | A031 | A035 |

The nested ACK handler now returns the output pointer saved with the
original call. Native nested ACKs carry offset zero even when the original
request lives at a nonzero offset, so recomputing the reply from the ACK
corrupted the returned dimensions. Framebuffer dimensions come from firmware;
the H17G surface layout matches the native startup record.

## Boot and test boundary

`t8142-j813-display.dts` is an explicit RAM bring-up profile. The host boot
preflight preserves and validates the live DCP firmware carveouts and DART
mappings. The resident Stage 1 and installed boot files are unchanged.

Load `appledrm` with `live_discovery=1` after that preflight. This sends the
native RTKit INIT/HELLO discovery exchange while preserving the live DCP CPU
and protected DART state. The option is currently explicit. Do not treat
generic firmware adoption or arbitrary firmware versions as qualified.

Use `modetest -M apple -c -p -e` to discover current connector and CRTC IDs.
The measured run used `modetest -M apple -s 47@45:2560x1664-60@XR24 -v`;
those IDs are observations from that boot and must not be assumed permanent.

`trace_rpc` enables bounded RPC diagnostics. `trace_calls=1` additionally
retains AP call logging after that budget is exhausted. Normal desktop use
leaves both disabled. The final opt-in logging change is build-validated;
the hardware result above used the same functional driver with verbose AP
call logging, subsequently suppressed on the live console.

Validation includes the ARM64 kernel/module build, an ASan/UBSan regression
for nested/main/empty/invalid ACK channels, malformed AFK layout checks, and
an exact 560-byte comparison between the C surface builder and the native
reference after removing transport poison. Native firmware, memory captures,
credentials and program bundles remain private and are not included here.

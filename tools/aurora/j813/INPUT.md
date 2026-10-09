# J813 native input

The built-in M5 keyboard is hardware-verified in Linux. A physical arrow-key
test produced 4,680 bytes / 195 `input_event` records, with DockChannel receive
interrupts advancing. The observation retained only an event count, not key
codes or typed text. This was desktop-v5 on 2026-09-10.

## Cold-start ownership

U-Boot's `apple_mtp_kbd` driver previously started MTP and shut it down before
Linux booted. Linux's subsequent restart crashed in the firmware's logging
state at PC 0x102a73c. The crash mailbox history contained both handshakes.
A direct, bounded m1n1 cold boot of the same firmware reached IOP/AP ON and
queued 1,032 bytes of DockChannel initialization data.

The pushed `u-boot/j813` commit `b0430a0846b` in `drivers/input/apple_mtp_kbd.c` skips this probe
only for `apple,j813`, before touching the helper or FIFO registers. With the
Linux kernel and DTB unchanged, MTP was stopped at Linux entry and then booted
successfully, enumerated STM, obtained its real identity, and registered the
keyboard. U-Boot keyboard input on J813 remains disabled until its warm
handoff is separately qualified. Other boards retain their existing behavior.

Qualified U-Boot binary SHA256:
`032fb201fda4366c209041ddd4e7ad6c9f1a5b50952178449e5a40f1ba20b274`.
The successful keyboard bundle is retained locally in `artifacts/desktop/v5`.

## Linux preparation

`t8142-j813-input.dts` extends the qualified Wi-Fi tree with live-ADT resources:

| Block | Address | Notes |
| --- | --- | --- |
| MTP ASC | 0x394600000 | CPU control and mailbox helper |
| MTP mailbox | 0x394608000 | AIC IRQs 1299 through 1302 |
| MTP DART | 0x394800000 | Stream 0; IRQ 1275 |
| MTP DAPF | 0x394810000 | Scoped preboot initialization |
| DockChannel IRQ | 0x394b14000 | AIC IRQ 1277 |
| Channel config/data | 0x394b30000 / 0x394b34000 | Channel 1 |
| Firmware SRAM | 0x394c00000 | 1 MiB iBoot carveout |

The DART uses the measured 32..288 MiB IOVA window. The J813 MTP helper opts
into sending AP ON before waiting for the IOP acknowledgment. Existing RTKit
callers retain their ordering. `boot-linux.py --input` validates the MTP/DART
addresses and initializes only MTP's DAPF; `--desktop` refuses an input DTB.

Use the J813 U-Boot guard when booting this DTB. A source checkout alone does
not update the binary consumed by `boot-linux.py`. On this Mac it is built
with GNU make, `CROSS_COMPILE=aarch64-elf-`, and Homebrew OpenSSL host include
and library flags. Linux kernel builds use the separate `j813-builder` VM.

## Trackpad hardware proof and combined desktop

Desktop-v6 adds the `multi-touch` node and explicit power method 2. J813 uses
firmware-managed reset: the existing nine-byte Will/Has messages and 50 ms
delay between ON Will and ON Has match the working Windows driver. The
measured J813 path has no GPIO_REQUEST and does not operate an AFE GPIO.
STM remains enabled for the real HID identity. On hardware, firmware loading,
power sequencing and the touch-ready notification all completed. The user's
physical movement/click test produced 208,608 bytes / 8,692 Linux input events.
Wi-Fi passed 5/5 pings and native Bluetooth startup completed 90 commands and
90 events with zero transmit/receive errors in the same v6 boot.

The private device-extracted HIDF firmware is staged locally as
`artifacts/private-firmware/tpmtfw-j813.bin`. The repacker verifies its SHA256
before placing it in the private initramfs at
`/lib/firmware/apple/tpmtfw-j813.bin`; no firmware is committed or placed in
the public desktop-root transfer directory. The Windows work's source copy
is `AuroraSilicon/build/apple-mtp-hid/tpmtfw-j813.bin`.

The keyboard and trackpad share their vendor/product ID. Loading magicmouse
alone prevents hid-generic from claiming the keyboard, even though magicmouse
rejects that interface in probe. The existing `hid-apple` driver handles the
keyboard. Desktop-v7 enables it and its LED-class dependencies, with required
configuration checks; no generic HID-core or magicmouse matching changes are
needed. `start-input.sh` loads both specific drivers before creating the HID
interfaces and checks that both platform drivers bind. Trackpad firmware is
started when its input device is opened. `start-desktop.sh` invokes this
helper when present.

Desktop-v7 boots with both specific drivers attached. Xorg/libinput recognizes
the keyboard and trackpad, and all four XFCE components remain running. A
120-second physical desktop test recorded 15,403 trackpad events (13,812
absolute, 116 key/button, 1,475 sync) and 483 keyboard events (162 key, 162
misc, 159 sync). `input-event-count.py` opens both devices without grabbing
them and retains only event-type totals, never key codes or coordinates.
The Xorg log also reported occasional discarded touch jumps; smooth motion,
gestures and palm rejection need further qualification.

The same boot passed DHCP and 5/5 external pings, followed by 3/3 while XFCE
was running. Native Bluetooth retained 90 commands / 90 events and zero TX/RX
errors; the private bond was restored to RAM. This is a startup regression,
not a new Bluetooth audio test. The desktop uses software rendering.

The terminal initially could not open a PTY because the initramfs had not
mounted devpts. Mounting it in the desktop namespaces fixed a real
unprivileged `pty.openpty()` test and produced a working terminal shell.
`start-desktop.sh` now mounts devpts before binding `/dev` and creating the
private session namespace, and checks PTY allocation before Xorg starts.
This userspace fix was applied to the running v7 and verified there, then
included in the next RAM image; the kernel did not change.

## GPU desktop input regression, 2026-09-13

A RAM boot reached XFCE with a stationary pointer. The MTP log showed an AFE
sensor boot failure (`No HINT_L`), then a touch-ready timeout. The transport
left its `starting` flag set after that timeout, so every subsequent open
returned `EINPROGRESS`; an Xorg udev reprobe could not recover it.

The timeout now clears `starting`, allowing a subsequent open to retry the
firmware/power sequence. `input-ready.py` performs bounded opens of both input
interfaces without reading events. The GPU shadow-Xorg launcher runs it
before Xorg probes libinput devices and fails startup if input remains broken.
The candidate builder copies the transport change and installs the helper.

After rebuilding and RAM-booting, both interfaces opened successfully, Xorg
registered the touchpad, and the user confirmed that the pointer moved while
GPU-rendered gears rotated. This boot succeeded on its initial sensor start;
recovery after an injected sensor failure has not been hardware-qualified.
The underlying intermittent AFE boot failure remains under investigation.

## Earlier evidence

- `logs/mtp-preboot-comparison2-20260910.log`: successful direct m1n1 boot.
- `logs/desktop-v5-cold-mtp-boot-20260910.log`: full RAM payload readback.
- `logs/desktop-v5-cold-mtp-input-probe-20260910.log`: cold Linux startup.
- `logs/desktop-v5-keyboard-hardware-count-20260910.log`: physical key events.
- `logs/desktop-v6-trackpad-hardware-count-20260910.log`: physical touch events.
- `logs/desktop-v7-demo-inventory-20260910.log`: combined physical event totals.
- `logs/desktop-v7-combined-check-20260910.log`: stable XFCE and 3/3 pings.
- `logs/desktop-v7-final-runtime-20260910.log`: Bluetooth startup regression.
- `logs/desktop-v7-terminal-retest-20260910.log`: unprivileged PTY proof.
- `logs/desktop-v7-frame2-20260910.png`: XFCE panels, icons and shell.
- `logs/standup-keyboard-working-20260910-receipt.log`: Discord HTTP 204.

Private SRAM captures support the diagnosis and stay outside version control.
`mtp-sram-snapshot.c` reads only the verified SRAM aperture. The optional
`probe-mtp-preboot.py` requires a fresh proxy boot and stops its MTP CPU when
finished; reboot before the next Linux attempt.

.. SPDX-License-Identifier: GPL-2.0-or-later

===================
Apple J700 keyboard
===================

The MacBook Neo (J700) uses the existing DockChannel HID transport and
hid-apple.  Its top row uses the same translation table as other recent
Apple keyboards, ``magic_keyboard_2021_and_2024_fn_keys``.  No separate
J700 table or driver is needed on the evidence currently available.
Hardware qualification of every key is still required.

Layout and identity
===================

Apple's `MacBook Neo keyboard guide
<https://support.apple.com/en-my/guide/macbook-neo/apdab672d5e9/mac>`_
documents these legends.  The Linux column describes the existing
hid-apple mapping, not a completed hardware test:

=====  ======================  ==================
Key    Legend/function         Linux keycode
=====  ======================  ==================
F1     Display dimmer          KEY_BRIGHTNESSDOWN
F2     Display brighter        KEY_BRIGHTNESSUP
F3     Mission Control         KEY_SCALE
F4     Spotlight               KEY_SEARCH
F5     Dictation               KEY_MICMUTE
F6     Do Not Disturb          KEY_SLEEP
F7     Previous                KEY_PREVIOUSSONG
F8     Play/pause              KEY_PLAYPAUSE
F9     Next                    KEY_NEXTSONG
F10    Mute                    KEY_MUTE
F11    Volume down             KEY_VOLUMEDOWN
F12    Volume up               KEY_VOLUMEUP
=====  ======================  ==================

KEY_MICMUTE and KEY_SLEEP are the existing Asahi equivalents, not a
promise of desktop dictation or Do Not Disturb.  In particular, an
ungrabbed F6 may suspend the machine.  Globe/Fn maps to KEY_FN through
hid-apple's vendor usage mapping; desktop emoji/input-source actions
are separate policy.

The unmodified 25G83 IPSW ADT has
``/arm-io/dockchannel-mtp/mtp-transport/keyboard/kblang-calibration``
set to the placeholder ``syscfg/ksku``.  It does not identify the test
unit's country or layout.  Asahi m1n1's ``dt_set_ipd()`` resolves the live
calibration into ``apple,keyboard-layout-id`` and ``hid-country-code``
at the Linux DT's keyboard alias.  Read these big-endian properties from
the running system; do not treat the DTS's zero defaults as a measured
ANSI layout.  Layout/country identify the regional keyboard, not its
F-key report usages.

The clean J700 DTS uses ``apple,no-stm``.  DockChannel consequently uses
the Apple host vendor ID with product/version zero.  The BUS_HOST
wildcard in hid-apple already matches this keyboard and selects the
modern table.  Product zero is not a J700-specific identifier.  Do not
add a product-zero quirk or replace the clean DockChannel driver to
manufacture an ID.

The ADT has no keyboard-backlight node; its ``/backlight`` node has
``dcp-brightness-node`` and describes display brightness.  The presence
of a PWM controller alone does not establish a keyboard LED.  No
keyboard LED is added.  Record the live LED inventory and physical
observation before declaring backlight qualification complete.  If a
keyboard LED is discovered, first identify its existing Asahi transport
and control mechanism rather than issuing guessed HID feature writes.

Fn modes
========

For this physical top row the existing ``hid_apple.fnmode`` behavior is:

====  ===================  ===================
Mode  Without Fn           With Fn
====  ===================  ===================
0     F1..F12              F1..F12
1     Media functions      F1..F12
2     F1..F12              Media functions
3     Media functions      F1..F12 (auto)
4     F1..F12              F1..F12
5     F1..F12              F1..F12
====  ===================  ===================

Mode 0 disables Fn translation.  Mode 4 disables synthesized F-keys
on Touch Bar devices; its name does not mean this physical row stops
emitting F-keys.  Mode 5 is the Asahi userspace-layout mode: it retains
Fn navigation translations but omits top-row translation on BUS_HOST.
Its ISO quirk adjustment happens at probe time; changing the parameter
at runtime tests the top row, not a fresh mode-5 regional-layout setup.
Changing Fn while an F-key is down must not change that key's release
code.  The manual capture includes a release-Fn-first case.

Manual Linux capture
====================

Use ``tools/testing/selftests/hid/j700-fnrow.py`` with Python 3, evtest,
coreutils and hid-tools (``hid-recorder`` and ``hid-decode``).  This is
an operator-driven hardware test, deliberately not in ``TEST_PROGS``.
It creates a new private output directory, checks both press and release
events, rejects extra events, and requires raw HID events for every
step.  Autorepeat is retained in raw logs but ignored in comparisons.
It does not require playback or write HID feature reports.

Run over SSH with a host-side interactive terminal.  Stop media players
and mute sinks first.  Prompts must be answered on the SSH host because
evtest grabs the internal keyboard throughout the capture.  That keeps
F6/F8/F10 away from desktop suspend/playback/mute actions.  Do not press
the separate power key.  Leave ``swap_fn_leftctrl=0`` for the test::

    sudo --preserve-env=SSH_CONNECTION python3 j700-fnrow.py \
        --out /var/tmp/j700-fnrow-clean --modes 0 1 2 3 4 5

Each step has a five-second window after the host operator presses
Enter.  Tap exactly the requested physical key, including Fn down/up
in each Fn chord, and release all keys before the next step.  The script
restores the original fnmode on normal exit, error, SIGINT or SIGTERM.
After a forced kill, restore the value recorded in ``inventory.json``
manually through ``/sys/module/hid_apple/parameters/fnmode``.

Output includes the input/HID identity, layout/country DT properties,
LED inventory, original module parameters, binary/decoded descriptor,
complete evtest and hid-recorder streams, per-step slices, and
``results.json``.  Record the kernel commit and boot/module artifact
identity alongside these files.  Review raw logs for identifiers before
publishing.  A PASS covers only the requested modes on that unit.

GNOME acceptance is a separate ungrabbed test.  On a session with a
working display-backlight provider, verify that F1/F2 change the actual
backlight value and show an OSD in modes 1/3, with Fn required in mode 2.
Check F11/F12 through ``wpctl get-volume`` with a muted sink, sound
feedback disabled, and no playback.  A temporary null sink avoids
speaker output even if GNOME unmutes on a volume-up press.  Keep real
hardware sinks muted and re-mute the test sink before every reading.
Do not test F10 ungrabbed on a hardware sink.  Missing display control
on a clean kernel is a display-provider dependency, distinct from a
correct KEY_BRIGHTNESS event.

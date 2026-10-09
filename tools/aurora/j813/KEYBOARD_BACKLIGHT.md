# M5 keyboard backlight

The J813 keyboard LEDs were physically confirmed on 2026-09-13 after a
native Linux FPWM test at brightness 128/255. The Windows
`AuroraSilicon/drivers/AppleKbdBacklight` implementation supplied the M2
reference; the M5 addresses and power dependency came from its own ADT.
No macOS HV trace was required for this control path.

## Hardware evidence

- `/arm-io/fpwm0`: `fpwm,s5l8920x`, physical base `0x3a5040000`, size
  `0x4000`, gate 82. Its `kbd-backlight` child uses channel 0 and 25 kHz.
- PMGR gate 82 (`FPWM0`) maps to `0x380700330`. Ancestors 66 (`SIO`) and
  58 (`AFISOCNI2`) have `no_ps` set and therefore no physical gates to drive.
- The live power-state transition was `0x244` to `0x2ff`. PWM registers
  were read only after the ACTIVE target and actual fields agreed.
- At 24 MHz and a 40000 ns period, the Linux brightness arithmetic for
  128/255 yields ON=481 and OFF=479 cycles. ON, OFF, then CTRL=`0x4021`
  were written in the existing `pwm-apple` order. CTRL read back `0x4001`
  after its update bit cleared, with both cycle counts retained.
- The user confirmed that the physical keys were lit. All ten cores
  remained online during this initial live test.

The device tree uses the existing Apple PMGR and PWM drivers plus `pwm-leds`.
The input and GPU/DCP build configurations enable them. The default is
128/255; the standard interface is `/sys/class/leds/kbd_backlight`:

```sh
cat /sys/class/leds/kbd_backlight/max_brightness
echo 64 > /sys/class/leds/kbd_backlight/brightness
echo 128 > /sys/class/leds/kbd_backlight/brightness
```

Enable `CONFIG_GPIOLIB` alongside `CONFIG_PWM`, `CONFIG_PWM_APPLE`,
`CONFIG_LEDS_CLASS` and `CONFIG_LEDS_PWM`. This kernel's `leds-pwm` probes
an optional `enable-gpios` descriptor with `devm_fwnode_gpiod_get()`. When
GPIOLIB is disabled the stub returns `-ENOSYS`, which the driver propagates;
the first candidate consequently registered PWM but failed LED probe with
error -38. J813 needs no enable pin, but enabling the framework allows the
absent property to be handled as optional. No GPIO controller driver is
needed for this backlight.

The corrected fresh boot registered the PMGR, PWM and LED drivers and
automatically applied brightness 128/255. The LED class test passed values
0, 32, 128, 255, then 128, with hardware ON/OFF counts respectively
0/960, 120/840, 481/479, 960/0, and 481/479. The LED wrapper and power
controller therefore work without the earlier direct-register setup.
Automatic ambient-light adjustment, keyboard shortcuts and suspend/resume
are separate, unqualified functionality.

Final kernel Image SHA256:
`e8f13f0d4b569d3f555ec06edc55a64cf2bd7bb134c8b17122b535b1a77d4b7c`.
DTB SHA256:
`1593c564ad53dc4ee8d28750513d0c84710f483d470888104563d484d6b99c73`.

After this boot, all ten CPUs passed concurrent workers and 200 affinity
visits. XFCE/DCP and Zink/Honeykrisp gears passed eight animation/resize
cases. An initial pixel check sampled the gears window while a maximized
htop terminal covered it; hardware submissions kept advancing. Bringing
gears forward resolved that capture-precondition failure, and the visible
test passed before focus was returned to htop. Wi-Fi passed 3/3 pings after
the desktop download completed. Htop 3.4.1 ran from the rebuilt root image,
with all ten CPU meters visible. Brightness remained 128/255.

Workbench evidence: `logs/m5-fpwm-power-state-20260913.log`,
`logs/m5-fpwm-enable-and-baseline-20260913.log`, and
`logs/m5-keyboard-light-first-on-20260913.log`. Standard driver evidence:
`logs/m5-keyboard-light-final-boot-20260913.log` and
`logs/m5-keyboard-light-led-class-proof-20260913.log`.
Desktop regression evidence: `logs/m5-keyboard-light-cpu-network-final-20260913.log`
and `logs/m5-keyboard-light-gears-visible-regression-20260913.log`.

The updated binding documents pass `dt-doc-validate`. Scoped DT validation
of the complete GPU/DCP tree emits the same 18 existing PMGR fallback
compatibility errors as the retained ten-core baseline; it adds no errors
for the keyboard PWM or its power-state node. The full tree is not claimed
schema-clean. The AVE include shares the main PMGR node and preserves its
existing register window and power-domain children.

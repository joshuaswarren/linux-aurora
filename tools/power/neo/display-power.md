# J700 display power investigation

The current development boot uses the inherited simple framebuffer, named
`softpipedrmfb`. There is no registered backlight and no bound DCP DRM driver.
Blanking the framebuffer cannot establish that the panel is powered down.

## DWI qualification, 2026-09-14

The ADT describes `/arm-io/dwi` at CPU physical `0x300200000`, DWI version 1,
and PMGR clock gate 13 (`ps_dwi`, main PMGR offset `0x118`). An initial raw
read covering DWI and DCP registers lost SSH and required Fedora Left-back
DebugUSB reset. No display-control writes preceded that failure; buffered
output did not establish which read stalled. Do not repeat unpowered reads.

The read-only `overlay/neo-dwi-probe.c` instead acquires the kernel-managed
power domain before accessing DWI, and retains it while bound. Build it with
the other external qualification modules. Compile `overlay/dwi-probe.dts`
without `-@` and load it through the manual overlay loader on J700 only.
The overlay requires the DWI provider/node to be absent from the live tree.
Its always-on domain is deliberate during qualification; remove it from the
measurement configuration before comparing power. This is an inventory tool,
not a backlight implementation.

On build #19 after recovery, PMGR reported DWI was **not on at boot**. With
the domain acquired, reads returned config `0x00000001`, lockout `0x3c`, both
command controls `0x70`, both command data registers zero, and strobe delay
zero. The panel was already illuminated. There is no inherited brightness
command to restore, so no DWI brightness write was attempted.

Native `AppleS8000DWI::sendWLED` at `fffffe00095ae8f0` uses immediate control/
data offsets `0x80/0x84` or asynchronous `0xc0/0xc4`, with command
`0xa0000000 | (brightness & 0x7ff)` for version 1. Control is `0x71` plus the
slave-select bit; completion clears bit 0. These offsets differ from the old
Linux DWI backlight aperture. This alone does not prove DWI drives Neo's panel.

The J700 ADT also describes EXBright and DCP-based brightness ownership.
The initial DWI inventory did not establish a panel off/on path. The later
X3200 investigation below identifies a separate LCD-enable control.

## Firmware and SMC inventory

The product ADT identifies a DP855 TCON, firmware 1.44 and bundle 9.19. The
panel is 2408 x 1506. `/backlight` selects `use-AAB-architecture` and
`dcp-brightness-node`; `/exbright` assigns brightness to EXBrightCore with an
exclave-owned MMIO window. These properties do not establish an AP-accessible
brightness register. iBoot also references `arm-io/spi3/dp855`; do not infer
that a generic external DisplayPort AUX sequence controls this panel.

`overlay/neo-smc-inventory.c` records key names and metadata through the bound
SMC driver, without reading values or writing keys. On J700 build #19 it
enumerated 2333 keys. Capture the journal rather than only dmesg: this inventory
can wrap the printk ring. DPBS/DPBW and the PDCP/PDTP/PDTR power telemetry keys
are not identified display-enable controls.

The matching DCP image is `Firmware/dcp/t8140dcp.im4p` from the 25F84 restore,
UUID `27E9ECCA-FA18-3D4D-8AFA-CC203DEB2A31`, matching the live ADT. Extracted
Mach-O SHA256:
`091e9d9320818360d422c938bde5e4fd5c4e83ccb6b44e2134fac907b975ae9f`.

The image includes several prototype panel configurations. Neither the
1520 x 1520 configuration at file offset `0x4d3e0c` nor DispTargetX1769's
2560 x 1600 configuration at `0x4d4508` identifies J700. In particular, do not
copy X1769's `gP13` LCD enable: J700's Linux DT and live GPIO consumer identify
SMC GPIO 19 as the touchpad AFE reset. Generic firmware strings are not a board
wiring map.

Two offline helpers retain reproducible analysis:

* `native/firmware-rebase-macho.py` resolves the DCP format-7 chained pointers
  through VM-to-file mappings. Ghidra otherwise interprets chain starts as file
  offsets across a zero-fill gap and fails. The qualified image has 29539
  pointers; the analysis copy SHA256 is
  `97f0d0a9961c6d7a067356c18b8c9a9e22cfc2ef91a9b68572a7999fc576c8aa`.
  Never install this modified analysis copy as firmware.
* `native/decode-osserialize.py` decodes saved binary configuration objects,
  including symbol and object-reference tags, and records the source hash.

Firmware functions `0x040753e0` (AUX transaction), `0x04105cec` (register
read), and `0x04105bb8` (register write) are investigation leads, not qualified
AP MMIO operations. Their virtual-device register offsets must not be added
to a guessed physical base. Backlight implementations at `0x040958b8` and
`0x040a26d4` write two brightness bytes through another virtual interface;
the concrete controller and J700 selection remain to be established.

## Measurement boundary

The completed screen-on 600-second run lost 121 mAh at a mean endpoint
voltage of 4.099 V across 601.665 seconds: approximately **2.968 W**. The
awake sampled baseline was **3.617 W**. No screen-off result exists yet.
The powered DWI probe may leave its temporary always-on domain enabled;
reboot into the clean measurement configuration before a comparison.

Before running the requested screen-off comparison, establish a reversible
panel/backlight operation, qualify a short off/on cycle with power evidence,
then hook it into sleep and wake. Black framebuffer pixels, successful RTC
wake, or a firmware transport acknowledgement alone do not establish that
the panel was powered down.

## X3200 LCD enable, 2026-09-14

The J700 BuildManifest identifies `Baobab,TCON` as
`Firmware/DP855/X3200/X3200.bin`. DCP's static initializer calls the
DispTargetX3200 name helper at `0x0435dc88`, then deserializes the object at
VA `0x0445c240` (file offset `0x45d240`), size `0xa04`. This configuration
matches the live panel's **2408 x 1506** timing and specifies:

```
lcd-enable: { key: gP16, on: 1, off: 0 }
lcd-off-on-ms: 260
```

The existing macsmc GPIO driver maps `gP16` to line 22 of the
`macsmc-pmu-gpio` chip. A GPIO character-device query found the line unclaimed,
already configured as an output, and high. `panel-gpio.c` uses that existing
driver; it does not access SMC transport registers or change pin mode. Its
`--read` operation preserves the direction. `--cycle SECONDS` accepts only
J700, the expected chip/line, and inherited output-high; it holds the line,
switches it low, then restores high. The helper respects at least 300 ms off
even on SIGINT/SIGTERM. SIGKILL and machine failure still require recovery.

The first 10-second awake cycle, boot
`222e9ec0-d1d1-4348-bf74-ace919ce5b38`, returned zero, read low during the off
interval, and restored high. Battery `power_now` settled at:

* Before: 3.578 W (16 half-second samples).
* Off: 2.8944 W (15 half-second samples after settling).
* Restored: 3.5235 W (17 half-second samples after settling).

This provides electrical evidence of approximately **0.68 W** removed by the
LCD-enable control. A compositor screenshot alone cannot prove physical
panel output after restoration. The 10-minute sleep comparison remains a
separate experiment, not this short awake cycle.

The earlier DWI WLED trial (`overlay/neo-dwi-trial.c`, `dwi-power-step.py`)
completed native version-1 immediate commands at 1024, zero, and 1024, but
power remained 3.58 W at every level. It is not a J700 backlight driver.
The trial domain is no longer forced always-on; remove the trial and its
overlay before measurements. The read-only `spmi-display-read.c` received
valid SPMI replies from both candidate buses, but register `0x1e00` was zero
on each while the panel was illuminated. Neither candidate is qualified for
brightness writes.

Additional offline analysis extracted the T8140 ExclaveCore shared cache
from 25F84. SHA256:
`2d227b8d7129bf7a11bd7798f122bf64cb14615932f6341aed6df2efeb2099f7`.
`native/exclave-rebase-macho.py` resolves its 73974 format-12 chained pointers
for analysis, including VM/file gaps. This is analysis tooling only.

Native reference: Darwin 25.5 / XNU 12377.121.10~1, RELEASE_ARM64_T8140;
kernelcache SHA256
`c4de7d65a016ff68f6ac2a335517da82c9be966175c80afd8a738dd00faa1c61`.
Workspace evidence lives under `artifacts/neo-suspend/display/`, including
ADT inventory, extracted kexts, disassembly, decompilation and powered reads.

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
Further investigation must establish the actual panel off/on path before a
screen-off sleep test can be claimed. No display power-down is qualified yet.

Native reference: Darwin 25.5 / XNU 12377.121.10~1, RELEASE_ARM64_T8140;
kernelcache SHA256
`c4de7d65a016ff68f6ac2a335517da82c9be966175c80afd8a738dd00faa1c61`.
Workspace evidence lives under `artifacts/neo-suspend/display/`, including
ADT inventory, extracted kexts, disassembly, decompilation and powered reads.

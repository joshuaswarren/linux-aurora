# MT7932 host-interface sleep qualification

This is an opt-in J700 experiment on `eryk_neo_deepsleep`, not yet a
qualified firmware power-down path. `mt7932_fullmac.hif_s2idle=1` adds an
acknowledged host-interface suspend/resume exchange to the existing,
disconnected, D0-retaining s2idle path. Default is off. DMA memory, IRQ
service, PCI power and driver ownership remain retained.

## Native reference

Retained `artifacts/neo-suspend/components/native/AppleSunriseWLAN.arm64e`
SHA256 `7914176d1062f9c3c956c6f507b012a0b49f6a12710e50acad8acad8cda8da23`.
This host driver is a protocol reference; equivalence to Neo's currently
loaded radio firmware is not assumed. Symbol-bounded disassembly is in
`wifi-hif-exact.dis` and `wifi-resume-and-hif.dis` beside it.

* `_halPciePreSuspendCmd` at 0x10003e448 sends legacy CID 0xf6, set=true,
  generic response=false, body length 36. First four bytes: 02 01 01 01;
  remaining bytes zero.
* `_halPcieResumeCmd` at 0x100038580 sends the same command with first four
  bytes 02 01 00 00. Constants at 0x10046bcb8 and 0x10046bcb0 respectively.
* Dispatch entry at file offset 0x488ce0 maps EID 0xf6 to
  `_nicEventHifCtrl` at 0x1003038b8. This is not unified event 0x03.
* Its event body is 36 bytes after a 12-byte event header. In our RX framing
  the header starts at byte 24, body at 36, minimum packet length 72.
  PCIe interface is body[0]=2. Suspend body[3] is nonzero, with success
  requiring body[1]=body[2]=2. Resume has body[3]=0.

The separate HIF completion prevents unrelated generic events from proving
success. Direction and a nonzero sequence must match the pending command;
sequence zero permits firmware's unsolicited event convention. Malformed or
mismatched events cannot complete the wait. A three-second timeout aborts
suspend. Even a failed suspend request invokes an explicit resume rollback,
because a lost acknowledgement does not prove the firmware stayed awake.
Rollback failure leaves the netdev detached and marks its policy failed.

## Qualification order

1. Build and boot with the parameter off; retain the known-good GPU module.
2. Enable HIF only, then `pm-test.py --stage devices` (real device callbacks,
   five-second PM test delay). Capture full HIF event payloads and rollback.
3. Only after successful round trip, run RTC/display-off tests, retaining
   noirq interrupt counters, firmware health, Wi-Fi reconnection and battery
   accumulated-counter data. Recalibrate the counter for the new kernel.
4. Firmware ownership handoff is a separate step: native suspend disables
   IRQs and calls `_nicpmSetFWOwn` after HIF acknowledgement; native resume
   acquires driver ownership before restoring IRQs and sending HIF resume.
   The corresponding always-on ownership register is BAR offset 0xe0010
   (chip address 0x7c060010), write 1 for firmware, 2 for driver. Do not gate
   DMA memory or PCI power merely because these writes have been issued.

Hardware results will be appended after execution; a compiled Image alone
is not evidence of a working suspend protocol or reduced power.

## First hardware results

Kernel #21, Image SHA256
`18bf4c97e2e42a2ef73b6f41e15a6a8b4fcf711aebcc5cd6226f6331b88cd94a`, source `b6eaeba53`, retained GPU module unchanged.
The `devices-01` test passed: EID 0xf6 carried exactly 72 bytes with
02 02 02 01 for suspend (sequence 200), and 02 02 02 00 for resume
(sequence 201). Transport geometry checks passed and WPA2 reconnected.

`sleep-10` passed real timed s2idle with the LCD/backlight hook. Noirq interval
10.71185 seconds recorded **zero MT7932 interrupts** (IRQ 58 stayed at 2207),
versus roughly 2.83/s in the prior retained-only experiment. RTC IRQ rose by
one, all CPUs entered s2idle, and the display restored. This establishes
host-interface quiescence, not firmware ownership or a wattage saving.
Captures are in workspace `artifacts/neo-suspend/wifi-hif/`; the manifest
retains binary identities and the installation script saved the previous slot.

# J700 PMU alarm qualification

The SMC RTC implementation has no alarm operations. J700 instead describes a
`pmu,baku` on `/arm-io/nub-spmi0`, with an RTC alarm. The bus port translates
to physical `0x308714000`; ADT slave ID is 14, controller generation 3,
AIC interrupt 487, peripheral interrupt 2. The existing Apple SPMI FIFO
layout reads this port successfully. Do not conflate it with the HPM bus
at `0x308908000`.

Static reference: Darwin 25.5.0 / XNU 12377.121.10~1, RELEASE_ARM64_T8140,
kernelcache SHA256
`c4de7d65a016ff68f6ac2a335517da82c9be966175c80afd8a738dd00faa1c61`.
`AppleDialogSPMIPMURTC::programRTCAlarm(unsigned int)` at
`fffffe000968da44` clears pending alarm bit 0 at `0xf80c`, writes a 32-bit
little-endian absolute up-counter seconds value at `0xf808`, unmasks bit 0
at `0xf80e`, then writes monitor bit 0 plus enable bit 6 to `0xf800`.
Disabling masks bit 0 and leaves the monitor enabled. Alarm event is W1C.
The interrupt action at `fffffe0009682de4` calls `programRTCAlarm(0)`.

The 6-byte counter at `0xf802` is little-endian, shifted right once to get
32768-Hz ticks. Legacy scratchpad offset uses seconds at `0xf704` and a
15-bit fraction at `0xf715`, combined and sign-extended from 47 bits.
`_readCurrentOffsetTicks` at `fffffe000968ce3c` describes this layout.
Keep these offset registers read-only during qualification.

Awake reads on kernel #19 (`9a1b478a5` Image, boot ID
`c96d75a7-fc5d-41d5-8a2e-fd0fd584e962`) succeeded, with empty FIFO status
`01000100` and valid response command/slave/parity fields. At one sample,
counter seconds `19410037.393280` plus offset seconds `0x697fdfa1` and
fraction `0x28ec / 32768` matched the current Unix time. Alarm control was
`01`, event `00`, mask `00`: alarm disabled, event unmasked.

`pmu-read.c` reproduces the bounded reads via `/dev/mem` and refuses a
controller already claimed in `/proc/iomem`. `--status` only reads status;
`--rtc` sends SPMI READ commands, without writing PMU registers. Build with
`aarch64-linux-gnu-gcc -O2 -Wall -Wextra -Werror -static pmu-read.c -o pmu-read`.
Do not run it concurrently with binding or using a kernel SPMI driver.

Full local evidence, ADT values, disassembly and decompilation are retained
under workspace `artifacts/neo-suspend/rtc-alarm/`. The alarm interrupt and
suspended wake route remain unqualified at this checkpoint.

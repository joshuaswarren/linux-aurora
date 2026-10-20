# J873 (Mac mini M6) APCIe: Apple N1 radio and Ethernet

2026-10-20. Opt-in device tree, kernel configuration and driver change for the two PCIe devices of the J873: the Apple N1 "Centauri"
Wi-Fi/Bluetooth radio on port 0 and the built-in Ethernet function `lan0` on port 2. **Nothing here has run on a J873.** The baseline
`t8152-j873.dtb` is unchanged (its build matches the recorded `04129ba6...` dtb); only `t8152-j873-radio.dtb` carries the PCIe nodes.

| File | What it is |
|---|---|
| `arch/arm64/boot/dts/apple/t8152-j873-pcie.dtsi` | pinctrl `gpio0`, two PCIe DARTs, port 0 PIODMA, the `pcie@1cb0000000` controller with ports 0 and 2, the SMC GPIO provider |
| `arch/arm64/boot/dts/apple/t8152-j873-radio.dts` | `t8152-j873.dts` plus the fragment (built as `t8152-j873-radio.dtb`) |
| `tools/aurora/j873/radio/radio.config` | kernel fragment: `APPLE_CENTAURI=m`, `BT_CENTAURI=m`, `PCIE_APPLE=m`, `GPIO_MACSMC=m`, Ethernet candidates `TIGON3`, `AQTION`, `R8169` as modules |
| `tools/aurora/j873/radio/build-config.sh` | resolves thermal + desktop + radio configs with `allnoconfig` (no kernel build) and checks every radio symbol stuck |
| `tools/aurora/j873/radio/compile-check.sh` | compiles the enabled drivers with the cross gcc (no link) |
| `drivers/gpio/gpio-macsmc.c` | the SMC `pcIO` radio-power write becomes per-machine data; J873 gets pin 19 (ADT `pKW8("pcIO", 0x800000, 0x130000)`) |

Verified here: the radio dtb compiles with no new warnings; `build-config.sh` reports all 33 radio symbols resolved; `compile-check.sh`
prints `RADIO_COMPILE_CHECK_PASS` (gpio-macsmc, pcie-apple, apple_centauri, btcentauri, tg3, atlantic, r8169_main).

## Where each value comes from (live ADT, `/arm-io/apcie`, compatible `apcie,t8152`)

- Controller regs: ECAM `0x1cb0000000`, RC `0x18c000000`, port 0/1/2 `0x188028000`/`0x189028000`/`0x18a028000`. The ADT has 28 reg entries: 10 shared plus 3 ports x 6.
  The T8140 (Neo) has 7 shared. The M6 layout is the T8140's translated by `0x390000000 -> 0x188000000` (RC +0x4000000, port 0 +0x28000, PHY +0x7020000).
- Interrupts: `1571 1579 1587` are ports 0, 1, 2 (the driver maps the port IRQ by index) and `1566` is unused. MSI window: `msi-vector-offset 1879`; all ports share one window (the driver writes the same MSIMAP to every port), so one range of 32.
- DART: `dart-apcie0` `0x188000000` IRQ 1572 (mappers control, alpha, beta, piodma; SID 17 is the PIODMA stream), `dart-apcie2` `0x18a000000` IRQ 1588. `iommu-map` uses slot == SID as on the T8140: bridge slot 0, then the functions in
  enumeration order, so control/alpha/beta are SIDs 1/2/3 behind port 0 and `lan0` is SID 1 behind port 2.
- PIODMA (port 0): `0x188030000/34/38`, IRQ 1573; window addresses equal the T8140's (ECAM, 64-bit range `0xbc0000000`/0x20000000, 32-bit range `0xb80000000`/0x40000000).
- GPIO: `gpio0` `0x11a000000` (`gpio,t8101`, 224 pins, interrupts 433..439). Port 0: PERST pin 3, CLKREQ pin 0; port 2: PERST pin 5, CLKREQ pin 2. CLKREQ pin muxing is left as iBoot set it, as on the Neo.
- Radio power: port 0 `function-reset = pKW8("pcIO", 0x800000, 0x130000)` on the SMC. The Neo's is `0x800000, 0x80000`; `gpio-macsmc.c` documents the encoding (low word, high word, level in bits 15:0, pin number in bits 23:16 of the high word), which gives pin 0x13 = 19 here.
- Power domains: none. The APCIe node's `power-gates`, `clock-gates` and `service-gates` are all empty in the ADT, and the J873 tree has no PMGR nodes.
- Functions: `pci-bridge0` has `centauri-builtin` and children `centauri-control` (fn 0), `centauri-alpha` (fn 1, Wi-Fi MAC), `centauri-beta` (fn 2); `pci-bridge2` has `lan0`.

## What to try, in order

1. Load `t8152-j873-radio.dtb` with a kernel built from `thermal + desktop + radio` configs. Load order in the RAM rootfs: `macsmc` (MFD), `gpio-macsmc`, then `pcie-apple`, then `apple_centauri`, `btcentauri`, and `tg3`/`atlantic`/`r8169` once you see which ID enumerates. `pinctrl-apple-gpio` and `apple-dart` are built in.
2. `dmesg | grep -i -E "pcie|apple_centauri|dart|pwren|pcIO"` and `lspci -nn`. Expected if everything above holds: `106b:1900`, `106b:1901`, `106b:1903` on bus 1 and one Ethernet function on bus 2; `apple_centauri` logs `ACIPC endpoint reachable: stage=...`. That line proves BAR access and a 40-bit DART DMA allocation, not Wi-Fi.
3. The Ethernet vendor/device is not in the ADT. The kernelcache has three NIC families that could be `lan0`: Broadcom BCM57xx (`pci14e4,16xx`), Aquantia Aqtion (`pci1d6a,*`) and Realtek RL (`10ec:8124`/`8125`); the config enables the Linux driver of each as a module. Read the ID from `lspci -nn` and drop the other two.

## Known blockers and unproven items (read before the first load)

1. **m1n1 has no `apcie,t8152` support, and the M6 ADT does not fit its T8140 path.** `src/pcie.c` stops at `pcie: Unsupported compatible`. Even mapped onto the T8140 code, its preflight needs five tunable properties the M6 ADT does not have
   (`apcie-axi2af-tunables`, `apcie-phy-ip-pll-tunables`, `apcie-phy-ip-auspma-tunables`, `apcie-cio3pllcore-tunables`, `apcie-pcieclkgen-tunables`; the M6 node carries only `apcie-phy-tunables` (13 entries) and `apcie-gapf-ocla-tunables`),
   expects 7 shared regs where the M6 has 10, and requires `maximum-link-speed` on every bridge (`pci-bridge2` has none). The Linux driver relies on m1n1 having initialised the PHY and PLL, so a T8152 init sequence in m1n1 is a prerequisite for any link. This is reverse-engineering work, not a configuration change.
2. **Gen3 DART.** `dart,gen3` (version 0x300, IAS 42) is exercised in this tree only by the PMP DART. The PCIe DARTs use the `apple,t8110-dart` compatible and may not probe or may place the IOVA window at `0x10000100000` (above the 40-bit mask the Centauri driver requests).
3. **`rom_boot` and `btcentauri` are still gated to `apple,j813`** and to private J813 firmware and calibration (`apple/centauri/j813-ftab.bin`); the board/chip word `0x00248142` is the J813's. The M6 values are unknown. Until they exist, expect enumeration and the `ACIPC endpoint reachable` line only.
4. Port 2's PIODMA (`0x18a030000`, IRQ 1589) is not described: the driver takes one `apple,piodma`. Port 1 and 2's PHY addresses (`0x18f024000`, `0x18f028000`) are extrapolated at the T6030's 0x4000 stride; only `phy0` (`0x18f020000`) is an ADT entry.
5. The `pcIO` pin number is derived from the documented encoding, and the write has not been tried on a J873. A wrong pin would toggle another SMC output. On J873 the driver routes only GPIO 19 through the `pcIO` write; every other pin keeps its normal `gP` key behaviour.
6. `apple,valid-functions = <0x7>` applies only to bus 1 (the driver tests `busr.start + 1`), so it does not touch `lan0`.

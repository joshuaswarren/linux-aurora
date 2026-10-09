# M5 Air J813 native Linux workbench

The primary driver is `drivers/net/wireless/apple/centauri/` on
`linux-aurora/j813`. All companion repositories also use `j813`; see
[source locations and setup](SOURCE-REVISIONS.md).

**September 10, 2026:** [N1 Bluetooth](N1-BLUETOOTH.md) has hardware proof of
pairing, encrypted reconnection and AVDTP endpoint discovery. The
[desktop campaign](DESKTOP.md) now runs XFCE with a working terminal, Wi-Fi,
and native Bluetooth startup. [Built-in input](INPUT.md) is hardware-verified:
the combined v7 desktop recorded physical events from both the keyboard and
trackpad through Xorg/libinput. Fastfetch, a live Wi-Fi ping demo, and btop
are displayed on the M5. The U-Boot J813 guard preserves a cold MTP start for
Linux. GPU acceleration, Bluetooth audio playback, and full trackpad gesture
quality remain unqualified. All payloads are RAM loaded.

**September 9, 2026: N1 Wi-Fi completes DHCP and bidirectional traffic.**
A macOS m1n1 trace exposed missing TX packet-preparation value 4; a Linux
0/4/0 crossover proved the fix. A fresh RAM boot with the normal TX queue
received a DHCP lease, passed router and incoming host pings, resolved DNS
and downloaded an HTTP page. One initial external ping was lost; the
follow-up passed 10/10. See [packet evidence](N1-DATA-RINGS.md) and
[current limitations](N1-PROGRESS.md). The running target remains in native
Linux with Wi-Fi connected.

Hardware result, September 8, 2026: native Linux 7.1.6-aurora-j813 boots
through U-Boot 2026.07 into the BusyBox initramfs shell. Interactive serial
commands and a two-second sleep/uptime check passed. AIC3 reports 2400/4096
interrupts; the architectural timer runs at 1 GHz; the internal 2560x1664
framebuffer registers as simplefb. Linux reports 16,019,680 KiB of memory.
Only the boot CPU is enabled for this milestone.

Evidence: `logs/linux-boot-05.log` contains U-Boot, Linux, and userspace startup.
`logs/linux-shell-proof.log` contains interactive command output, advancing
uptime, and confirmation that only RAM filesystems are mounted. The initial
capture script was interrupted after it failed to recognize the `~ #` prompt;
the separate proof script then verified the running shell successfully. The
prompt detector in `boot-linux.py` has since been corrected.

This workbench builds and RAM-loads native Linux through m1n1 and U-Boot.
The installed resident boot image and internal disks are preserved. The
initial device tree covers CPU, AIC3, DockChannel/Samsung serial, and the
firmware framebuffer. Storage, USB host, GPU acceleration, and SMP are outside
this initial boot configuration. The separate `wifi` configuration adds the
measured PCIe/DART path and the experimental N1 module; see `N1-PROGRESS.md`
for current proof.

Sources:

- Linux Aurora `drm-agx3`, Linux 7.1.6, base
  `4ac2664258f370cf7b03327da742b5885f91ca0b`, isolated local branch `j813`.
- U-Boot Aurora `j516`, base
  `b62ab39bc2dfacf1d22ca7a415ad2bc68536bfa1`, plus the local T8142 memory map.
- m1n1 Aurora `j813`, base
  `69534d4899e20108d2c7a085565a7097509fa17f`, plus the local minimal boot path.

No existing Linux Aurora branch included a J813/T8142 device tree when the
19 live branches were inspected. `linux/arch/arm64/boot/dts/apple/t8142-j813.dts`
uses addresses measured from this target's Apple device tree. The boot CPU's
MPIDR was measured separately: its affinity is `0x10100`, not ADT reg `0x100`.
Cold secondary CPU nodes are pruned by m1n1 for the first boot.

The `aurora,minimal-boot` property selects two local m1n1 changes:

- Skip the legacy EL2 ADT handoff, which faults through an invalid mapping
  after FDT preparation. Native Linux consumes the FDT.
- Skip generic PCIe/DAPF setup, which touches unsupported T8142 registers.
  The Wi-Fi payload instead uses measured N1 preboot setup and Linux DART.

U-Boot's new T8142 map uses the measured I/O range from `0x210000000` through
`0x600000000`; RAM and framebuffer are filled from m1n1's prepared FDT.

The local Linux AIC3 change recognizes `apple,t8142-aic3` and masks guest
timer interrupts through the architectural EL02 timer controls. T8142 traps
writes to the legacy `VM_TMR_FIQ_ENA_EL2` register. The previous kernel panic
identified that exact write in `aic_init_cpu`; the replacement follows the
existing m1n1 T8142 implementation while preserving older SoC behavior.

Build the kernel and initramfs in the dedicated Lima Linux builder:

```sh
limactl shell j813-builder bash /Users/mcruz/Developer/aurora-silicon/j813-linux/build-kernel.sh
```

Build the Wi-Fi kernel, module, firmware-containing initramfs and network tools:

```sh
limactl shell j813-builder bash /Users/mcruz/Developer/aurora-silicon/j813-linux/build-kernel.sh wifi >> /Users/mcruz/Developer/aurora-silicon/j813-linux/logs/kernel-wifi-build-02.log 2>&1
```

Watch build progress:

```sh
tail -f /Users/mcruz/Developer/aurora-silicon/j813-linux/logs/kernel-wifi-build-02.log
```

After `run-chainload.py`, use `boot-linux.py --wifi` for this payload.
For the qualified single-link WPA2 setup, run the following once at the
fresh target Linux shell. Firmware and ring setup still use the experimental
debug interface:

```sh
modprobe apple_centauri station_link_addresses=1
d=/sys/kernel/debug/apple_centauri/0000:01:00.0
for n in 1 2 3; do
    echo "$n" > "$d/rom_boot" || break
done
ip link set wlan0 up
for n in 76 24 15 12; do
    echo "$n" > "$d/data_probe" || break
done
iw dev wlan0 set power_save off
iw reg set US
echo 1 > "$d/host_wakeup"
echo 1 > "$d/tx_config"
echo 2 > "$d/tx_config"
```

From the host workbench, run `configure-wpa.py --disabled`. This installs
credentials from the protected local input into the target's RAM filesystem
without logging them. Then at the target Linux shell:

```sh
wpa_supplicant -B -D nl80211 -i wlan0 -c /run/n1-wpa.conf -f /run/n1-wpa.log
wpa_cli -p /run/wpa_supplicant -i wlan0 enable_network all
wpa_cli -p /run/wpa_supplicant -i wlan0 status
```

Wait for `wpa_state=COMPLETED` before opening the normal TX queue and obtaining
a lease. Use a new capture label if repeating the DHCP helper:

```sh
echo 25 > /sys/kernel/debug/apple_centauri/0000:01:00.0/data_probe
sh /bin/n1-dhcp-test first-lease
ip -4 addr show dev wlan0
ip route show
```

`data_tx_native_prepare` is enabled by default. The broadcast-only diagnostic
queue is disabled by default and is not needed. The explicit
`station_link_addresses=1` setting remains part of the qualified setup.
The test helper configures address, route and DNS in RAM; it exits after the
lease and is not a persistent DHCP renewal service. Automatic firmware/ring
startup and long-term connection management remain future work.

After module-only changes, rebuild with `build-n1-module.sh` and refresh the
existing private Wi-Fi RAM image with `repack-n1-initramfs.sh`, both inside
`j813-builder`. The repacker updates `artifacts/wifi/SHA256SUMS`.
The private firmware/calibration-containing initramfs must not be published.

Native build commands, run inside the respective isolated worktrees:

```sh
gmake -j12 RELEASE=1 BUILD_DIR=build-j813-linux
gmake CROSS_COMPILE=aarch64-elf- apple_m1_defconfig
gmake -j12 CROSS_COMPILE=aarch64-elf- HOSTCFLAGS='-O2 -I/opt/homebrew/opt/openssl@3/include' HOSTLDFLAGS='-L/opt/homebrew/opt/openssl@3/lib'
```

`run-chainload.py` RAM-loads the local m1n1 image and verifies the target chip.
`boot-linux.py` uploads and reads back each payload, boots through U-Boot,
captures native console output, and checks for a working initramfs shell.
Both scripts require the AuroraDbg virtual environment's Python.

The M5 uses host HPM RID 1 and `/dev/cu.kis-01100000-ch-0`. The existing Neo
session uses RID 0 and must not be selected for this task. The task-local
`macvdmtool/macvdmtool --rid 1 debugusb` enables the M5 console and requires
administrator authentication. For a halted payload, `--rid 1 reboot debugusb`
returns it to the resident proxy. Do not use the installed fixed-port helper
for M5 recovery while the Neo session is attached.

Progress reporting contract:
`/Users/mcruz/.codex/attachments/877c20f2-0018-42bb-9097-021833f19a67/pasted-text.txt`.
Reread the full contract after context loss. The webhook is supplied only via
the runtime `AGENT_PROGRESS_WEBHOOK_URL` environment variable; it is not stored
in this workbench. `post-progress.py` posts milestone-only Components V2 updates.

Captured evidence is under `logs/`; built payloads are under `artifacts/`.

# J813 XFCE and GPU campaign — 2026-09-10

User objective: match the J700 Wi-Fi, Bluetooth, XFCE and GPU milestones.
Primary reference task: `01a08150-b0a1-7f00-9430-1125685d499f`, titled
`Macbook Neo Linux Work`. Its latest observed state has framebuffer XFCE,
working Wi-Fi, and hardware Vulkan compute. Its 06:02 UTC update reports that
the accelerated desktop test exposed a GPU timeout and Mesa software fallback;
that attempt was stopped and is not an accelerated desktop pass. Read-only reference
files live under `../neo-linux/`; do not control or modify the Neo from here.

## Published Bluetooth checkpoint

`linux-aurora/j813` commit `c0f6eceacec7` is pushed to GitHub. It contains the
experimental N1 driver, bounded diagnostics, RAM-boot tools, and recorded
hardware pairing/encrypted reconnection/AVDTP results. Private captures,
firmware, calibration and credentials are excluded. Other repos had no new
Bluetooth implementation to commit. Existing unrelated dirty files remain.

Local `j813` subsequently merged `aurora/drm-agx3` at
`feaeef21ab812797fba5dca793644e80ce83f920` as `05e5fce7df69`, retaining original
history. This brings the same current AGX3 groundwork used by J700. T8142/G17G
still matches a diagnostic probe; only T8140/G17P enters its working runtime.
The M5 requires its own topology, firmware lifecycle and GPU submission proof.
No J700 identity or firmware has been substituted for it.

## Desktop and input status

Desktop-v3 resolved the black-screen session failure and showed XFCE panels,
icons and a terminal. Native physical keyboard events are now proven in v5,
and trackpad firmware startup and physical events are proven in v6. The root
cause and Windows-reference comparison are recorded in [INPUT.md](INPUT.md).
Desktop-v7 combines the dedicated Apple keyboard/trackpad drivers. Xorg uses
both through libinput; a physical test in the running desktop recorded 15,403
trackpad and 483 keyboard events. All four XFCE components remain running,
and a framebuffer capture shows panels, icons and a working terminal shell.
The same boot passed 5/5 and then 3/3 external pings; Bluetooth startup retains
90 commands/events with zero TX/RX errors. Rendering remains software-only.
Occasional discarded touch jumps are recorded; motion quality, gestures and
palm rejection still need qualification.

The first candidate keeps the firmware framebuffer and the single boot CPU.
The old kernel registered simplefb but had `CONFIG_FB_DEVICE` disabled, so
userspace had no `/dev/fb0`. `desktop.config` enables that interface and
desktop prerequisites including IPC, inotify, input events/uinput, cgroups,
namespaces and the existing Wi-Fi/Bluetooth drivers. GPU acceleration remains
a separate qualification step.

`build-desktop-root.sh` prepares a clean signed Ubuntu 26.04 ARM64 XFCE root
in `j813-builder`, with Xorg fbdev, BlueZ/Blueman, PulseAudio, htop and SSH tools.
It uses a fresh root, not Neo's installed user data. Its root account and
`aurora` user have no configured password; SSH host keys and machine IDs are
removed before archiving. The initial launch uses a private Xauthority cookie,
disables network X11 listening, and runs XFCE as the unprivileged user.

Root archive rebuilt on 2026-09-13 with htop 3.4.1-5build2:
`artifacts/desktop/desktop-root-v1.tar.gz`, 363318352 bytes,
SHA256 `ada4e031949b4985bc8ee9abeac64faf3a28178a59f9ba7a1e0b624f72e202bc`.
Package manifest and checksums are adjacent. Serve only this public root and
its manifest from a dedicated transfer directory; the parent also holds
private firmware-containing boot images and must not be exposed over HTTP.

`build-desktop-kernel.sh` uses a separate case-sensitive source/build pair in
the M5 VM. Source is archived from the local integration commit; the current
Bluetooth driver is an explicit, hashed overlay. The default builds desktop-v3;
the `input` flavor builds desktop-v7 with MTP/RTKit source overlays and checks
all required driver options. Image, DTB and module builds passed with W=1.
The initial full build's warnings were in existing core code; the later input
builds reported no warnings in the changed drivers.
The first stable-address build missed `linux/hex.h`; that include was added
and the complete rebuild passed. Do not use the earlier failed build result.

`repack-desktop-initramfs.sh` retains the private Wi-Fi firmware and replaces
every module with the matching desktop release, including the standalone
Bluetooth tool bundle. All boot hashes were checked after the final repack.
`boot-linux.py --desktop` selects these files with full KIS write/readback
verification and the already-qualified N1 PCIe preparation.

The new `native_bdaddr` module parameter accepts the saved diagnostic adapter
identity so a reboot can retain the AirPods bond. Without it, the original
temporary-address behavior remains. It rejects malformed/zero/broadcast input
before native commands and waits for secure randomness for the seed in both
modes. Hardware readback verified the same identity after reboot. It is retained
in private `logs/adapter-private-info-20260910.log`; the paired key remains only
in the private successful pairing capture. Do not print or publish either.

The initial desktop-v1 candidate booted with complete payload readback and a
verified serial shell. It exposes `/dev/fb0`, format x2r10g10b10, 2560x1664,
stride 10240. Wi-Fi reinitialized and passed 5/5 Google pings. Bluetooth native
startup and HostAwake succeeded with the saved adapter address and zero errors.

The 352767782-byte clean root transferred through the M5's Wi-Fi, matched its
expected SHA256, and extracted into tmpfs `/desktop`. The temporary public HTTP
server was then stopped. `restore-bt-bond.py` validates the private successful
pairing capture and can restore its exact key into RAM BlueZ storage without
printing it; it also saves the diagnostic address module option in that RAM
root. This is restored test state, not standalone persistent bond storage.

`start-desktop.sh` launches Xorg in a chroot and XFCE in a private mount
namespace while preserving the running network and kernel instances. See the
v3 proof and current input status below. The console's interactive line length is
bounded: an oversized setup command was truncated before execution, then the
continuation prompt was canceled. `start-n1-desktop.sh` now carries that setup
as a verified uploaded file. `linux-command.py` rejects commands over 900 bytes
before opening serial; the rejection path was tested without hardware access.

## Desktop-v3 fixes and hardware proof

The black-screen failure is resolved in desktop-v3. The kernel enables
`CONFIG_SYSCTL` and `CONFIG_PROC_SYSCTL`; the launcher gives `/run/user` and
`/run/dbus` mode 0755, preserves the private 0700 user directory, and creates
the standard root-owned 01777 ICE socket directory. The original console's
private umask had prevented the user session from traversing those parents.

Linux rejects a new user namespace from a chroot (`current_chrooted()`), which
also blocked the image-loader sandbox. `desktop-root-exec.sh` creates a private
mount namespace, pivots only that namespace to the RAM root, and detaches its
old root. A real unprivileged user-namespace test passed on the M5. The serial
console and radio processes remain in their original namespace. The image
loader sandbox remains enabled.

Desktop-v3 booted with complete payload readback, completed DHCP and 5/5 pings,
and transferred the clean root with a matching SHA256. The session, window
manager, panel and desktop remained running past the 20-second startup check.
A verified framebuffer download shows panels, icons and the launcher at
2560x1664. `xfce4-terminal` also launched; another 3/3 external pings passed.
The black wallpaper is the current default, not a failed session. Rendering
is software-only and no physical input is present in this v3 device tree.

Evidence: `logs/desktop-v3-{boot,network,display-proof,app-diagnosis}-20260910.log`
and `logs/desktop-v3-frame-20260910.png`. The green XFCE milestone post delivered
HTTP 204; see `logs/standup-xfce-working-20260910{,-receipt}.json`.

## Input candidate

The v7 terminal test exposed a missing devpts mount. The launcher now mounts
devpts before binding `/dev` and creating the private desktop namespace, and
checks unprivileged PTY allocation. The equivalent live fix passed on v7 and
the framebuffer shows an interactive shell. The corrected launcher is in
the repacked private RAM image; that repack has not required a kernel change.

`show-desktop-demo.sh` opens Fastfetch, live N1 Wi-Fi pings and btop as the
desktop user. Install `fastfetch btop` in the RAM root first and permit that
user's group to use ICMP echo sockets (`1000 1000` in
`/proc/sys/net/ipv4/ping_group_range`). Copy the script into
`/desktop/usr/local/bin/j813-desktop-demo`, make it executable, and launch it
through `j813-desktop-root-exec` as `aurora` with the existing DISPLAY,
XAUTHORITY and runtime-directory environment. The Wi-Fi ping binds explicitly
to `wlan0`; no traffic is simulated. Fastfetch reports software rendering
explicitly. Packages were installed over the M5's Wi-Fi, and all three windows
were inspected in a SHA-verified framebuffer capture. The original clean root
archive remains unchanged, so future boots need the demo packages installed.

The live kernel reports online, present and possible CPU masks all as `0`.
btop correctly shows one CPU: this minimal boot prunes stopped CPU nodes and
uses `maxcpus=1`. Showing every M5 core requires a separate native SMP boot
qualification, not a btop setting. The demo labels the measured online count.
The Wi-Fi driver does not yet implement `iw link` signal/bitrate statistics;
the demo labels those unavailable while displaying actual live ping replies.

The btop demo uses one full-height CPU chart, block glyphs and one-second
updates. Initially the CPU graph was nearly flat because measured utilization
was only 0.33%, not because CPU accounting had stopped. A bounded 20-second
real workload produced 100% measured utilization and a visible chart spike;
the workload then exited. The demo retains the normal 0-100% scale and does
not manufacture background load. Evidence is in
`logs/btop-cpu-{investigation,load-proof,recovery}-20260910.log` and
`logs/M5-btop-CPU-load-20260910.png`.

Evidence: `logs/desktop-v7-showcase-20260910.png` and
`logs/desktop-v7-demo-ping-permission-20260910.log`.

`build-desktop-kernel.sh input` layers `desktop-input.config` and the separate
`t8142-j813-input.dts` onto the Wi-Fi/desktop foundation. It enables the mailbox,
RTKit helper and DockChannel HID modules, including their `CONFIG_PM`
dependency, without changing the qualified Wi-Fi-only DTB. The source builds
with W=1 and required-option checks. The first v4 bundle reached the controller
startup failure described below; the cold-start fix and physical event proof
are recorded in [INPUT.md](INPUT.md).

MTP resources were checked against the live ADT alongside the already-proven
AIC and UART addresses. The MTP ASC is at 0x394600000, DART at 0x394800000,
DockChannel IRQ at 0x394b14000, and channel-1 config/data at
0x394b30000/0x394b34000. The mapper uses stream 0. The separate 1 MiB firmware
window at 0x394c00000 is consistent with the captured iBoot segment ranges and
prior J813 MTP handoff. Trackpad firmware/power sequencing remains a separate
step; no J700 firmware, GPIO mapping or device identity is substituted.

The v4 mailbox bound and exchanged RTKit protocol-v12 messages, proving native
interrupt-driven communication. MTP then crashed during initialization with a
reported PC 0x102a73c / ESR 0x96000004 and helper timeout -62; Linux remained
responsive. No repeat firmware submission was attempted in that boot.

The user pointed back to the already-working J813 Windows input path. Comparison
with `AuroraSilicon/drivers/AppleMtpHid` and m1n1's `src/mtp_handoff.c` found
three missing details: scoped MTP DAPF initialization, the low 32..288 MiB IOVA
pool, and AP=ON announced before waiting for IOP=ON. The next v5 candidate
includes all three and the STM child required to obtain the real HID identity.
The RTKit early-AP entry point is selected only by the J813 MTP helper; existing
callers retain their ordering. V5 still failed until the J813 U-Boot guard
removed the earlier boot/shutdown cycle; cold Linux startup and physical
keyboard events then passed. Use `boot-linux.py --input`, which checks the MTP/DART addresses and
prepares only that DAPF block. `--desktop` rejects an MTP-bearing DTB.

The user reports that M5 keyboard and pointer input already worked through AIC3
in the Windows driver work. Treat those source and hardware results as the
primary J813 reference. J700 contributes desktop and shared-driver lessons,
not substitute firmware, GPIO values or identities.

The initramfs repacker now includes only modules for its own kernel release;
saved v1/v2/v3 bundles and module directories remain available outside it.

## Coordination and reporting

### Desktop-v1 failure and v2 candidate

The v1 root downloaded, verified and unpacked successfully. The private bond
restore also completed successfully before Xorg was started. Xorg recognized
the framebuffer at depth 30 / 32 bpp, then crashed during screen setup after
the DPMS initialization message. This was a userspace crash; no kernel or
radio fault appeared. Its exact cause remains unresolved.

The RAM root's package manager then returned permission denied acquiring its
frontend lock even though root ownership, effective capabilities and a direct
write test all passed. The configuration lacked `CONFIG_FILE_LOCKING`; the
kernel's disabled `fcntl_setlk` implementation explicitly returns EACCES.
That identifies the package-lock failure independently of the display crash.

Desktop v2 adds file locking, file handles, memory-advice calls, rseq,
membarrier, memfd, core dumps and AIO. It selects SimpleDRM and fbdev emulation
with a conventional 32-bpp command-line surface, using the same display route
as J700. `desktop-fbdev.conf` is back to depth 24 for this route. The v2
Image/DTB/modules built, and the final initramfs hashes passed. It booted with
full payload readback, a working serial shell, and SimpleDRM fbdev emulation.
The old v1 boot bundle is retained in
`artifacts/desktop/v1`; the earlier qualified Bluetooth bundle is also intact.

The booter now sets the desktop's UTC wall clock from the host after the shell
appears; RTC is not yet wired. `start-desktop.sh` opts udevadm into operating
in the RAM chroot. A separate debugger installation in the builder initially
failed because the clean root's exported resolver file was intentionally
empty; restoring the builder resolver made that package installation pass.
No root archive containing those later builder changes has been exported.

### Desktop-v2 hardware results

Wi-Fi completed DHCP and passed 5/5 pings to 8.8.8.8 with zero packet loss.
The clean 352767782-byte desktop archive transferred over Wi-Fi again, passed
its SHA256 check, and extracted successfully into RAM. The dedicated public
transfer server was stopped afterward. Bluetooth startup reported 90 commands,
90 events and zero TX/RX errors with the retained diagnostic identity; the
private test bond was restored to RAM before starting the desktop.

Xorg stays running with a 2560x1664, 32-bpp, stride-10240 framebuffer. A verified
framebuffer download shows a pointer on a black screen. XFCE briefly started,
then exited; its launch marker alone is not proof of a working desktop.
`/run/user` was created mode 0700 under the console's private umask, preventing
the unprivileged session from reaching its runtime directory. Bubblewrap also
fails because `/proc/sys/kernel/overflowuid` is missing in the minimal kernel.
The next candidate needs explicit runtime-directory modes and sysctl support,
followed by a full session and image-loader sandbox test. No hardware input,
GPU acceleration, or audio playback pass is claimed.

Evidence remains private under `logs/desktop-v2-*`. The first read-only live GPU
inventory is `logs/gpu-topology-live-20260910T060540Z.json`. Its `get_reg()`
addresses include parent-bus translation and differ from the pinned T8142 GPU
table by 0x210000000. Resolve raw versus translated address conventions before
any GPU MMIO or device-tree changes; this inventory does not validate GPU power
or firmware startup.

Only M5 RID1 and `/dev/cu.kis-01100000-ch-0` are authorized here. Neo uses RID0
and its task is active. Never overlap serial helpers or replace resident Stage1.
All changes so far are RAM payloads and local source/build artifacts.

The new campaign's amber standup post was delivered once, HTTP 204, in
`logs/standup-desktop-start-20260910{,-receipt}.json`. Do not duplicate it.
After context loss, first reread the full user contract at
`/Users/mcruz/.codex/attachments/0687450c-4282-4386-85fd-d65ce5f9259a/pasted-text.txt`.
Never recover the webhook from files or include its value in artifacts.

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

## Desktop kernels booted; full XFCE session still failing

The first candidate keeps the firmware framebuffer and the single boot CPU.
The old kernel registered simplefb but had `CONFIG_FB_DEVICE` disabled, so
userspace had no `/dev/fb0`. `desktop.config` enables that interface and
desktop prerequisites including IPC, inotify, input events/uinput, cgroups,
namespaces and the existing Wi-Fi/Bluetooth drivers. GPU acceleration remains
a separate qualification step.

`build-desktop-root.sh` prepares a clean signed Ubuntu 26.04 ARM64 XFCE root
in `j813-builder`, with Xorg fbdev, BlueZ/Blueman, PulseAudio and SSH tools.
It uses a fresh root, not Neo's installed user data. Its root account and
`aurora` user have no configured password; SSH host keys and machine IDs are
removed before archiving. The initial launch uses a private Xauthority cookie,
disables network X11 listening, and runs XFCE as the unprivileged user.

Root archive: `artifacts/desktop/desktop-root-v1.tar.gz`, 352767782 bytes,
SHA256 `f6fbe2e086c7df6e194ef01f98d1208e8aaac3fe51af96547ab33c97850ddd6d`.
Package manifest and checksums are adjacent. Serve only this public root and
its manifest from a dedicated transfer directory; the parent also holds
private firmware-containing boot images and must not be exposed over HTTP.

`build-desktop-kernel.sh` uses a separate case-sensitive source/build pair in
the M5 VM. Source is archived from the local integration commit; the current
Bluetooth driver is an explicit, hashed overlay. It builds
`7.1.6-aurora-j813-desktop-v2`. Both Image/DTB/module builds passed with W=1; 538
warnings are in existing core code, and none are in Centauri/Bluetooth.
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

The desktop-v1 candidate has now booted with complete payload readback and a
verified serial shell. It exposes `/dev/fb0`, format x2r10g10b10, 2560x1664,
stride 10240. Wi-Fi reinitialized and passed 5/5 Google pings. Bluetooth native
startup and HostAwake succeeded with the saved adapter address and zero errors.

The 352767782-byte clean root transferred through the M5's Wi-Fi, matched its
expected SHA256, and extracted into tmpfs `/desktop`. The temporary public HTTP
server was then stopped. `restore-bt-bond.py` validates the private successful
pairing capture and can restore its exact key into RAM BlueZ storage without
printing it; it also saves the diagnostic address module option in that RAM
root. This is restored test state, not standalone persistent bond storage.

`start-desktop.sh` launches Xorg/XFCE in a chroot while preserving the running
network and kernel instances. Built-in keyboard/trackpad wiring and visual
desktop proof are still pending. The console's interactive line length is
bounded: an oversized setup command was truncated before execution, then the
continuation prompt was canceled. `start-n1-desktop.sh` now carries that setup
as a verified uploaded file. `linux-command.py` rejects commands over 900 bytes
before opening serial; the rejection path was tested without hardware access.

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

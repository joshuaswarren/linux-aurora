# J873 RAM desktop bring-up

On 2026-09-23 the J873 target booted Ubuntu 26.04 userspace and Xfce on
Linux 7.1.6-aurora-j873-desktop. The user confirmed the desktop on the LG
32UQ750 connected directly over USB-C/DisplayPort. A framebuffer capture
shows the panel, desktop icons and hardware-status terminal. XTest input
to xfce4-terminal executed a command and produced a checked marker file.
This is application/session evidence; it does not prove physical USB input.

## Proven in this boot

- All 12 CPUs online after the staged thermal startup gate.
- 125 SMC temperature channels and passive thermal zones, both CPU clock
  domains capped at their conservative minimum OPPs, automatic fan operation.
- Xorg fbdev at 1280x720, Xfce session, panel, desktop, window manager and
  graphical terminal; software rendering.
- The enlarged initramfs was uploaded and read back before handoff. Its RAM
  staging area preserves the native m1n1 allocations and the separate DCP
  callback arena. No boot object or target disk was modified.

## Remaining work

Physical USB input is not working yet. The keyboard is in a rear port and
the Apple trackpad is in a front port. Linux USB host controller, PHY,
PMGR2 and Gen3 DART integration must be qualified before claiming input.
Native Linux DCP/DRM and J873 Mesa hardware acceleration remain untested.
Display currently inherits the m1n1 framebuffer. This is a root RAM session,
not a persistent installed system. Suspend and wall-clock initialization
are not qualified.

## Build

These scripts require the existing `lima-j813-builder` guest and exported
J873 kernel source at `$HOME/j873-linux-source` inside that guest.

1. `build-kernel.sh` creates a separate desktop kernel build and checks the
   required thermal, framebuffer, HID and userspace sandbox configuration.
2. `build-rootfs.sh` creates a new debootstrap root. It refuses to overwrite
   an existing one, and suppresses service startup during package install.
3. `pack-rootfs.sh` installs matching modules and startup scripts, then emits
   the initramfs and artifact checksums.

The first desktop kernel opened Xorg but lacked the namespaces, proc sysctl
and seccomp features needed by the modern sandboxed icon loader. Adding
those kernel features fixed the session exit. The minimal package set also
requires `dbus-system-bus-common` for the system bus configuration.

Private hardware evidence and guarded handoff scripts are under
`j873/installer-prep-26A428/desktop-boot2-20260923/`. The exact confirmed
artifacts are retained under `j873/linux-artifacts/desktop-confirmed-20260923/`.
Use the current native boot identity and its saved allocation state; addresses
from an earlier boot are not reusable.

# Neo X11 composited cursor

On J700, appledrm deliberately registers no legacy cursor plane. Xorg's
modesetting driver therefore removes and restores its software cursor during
overlapping redraws of the active scanout buffer. With KWin compositing and
glxgears, direct scanout sampling reproduced the visible gaps.

This helper copies the current XFixes cursor, including its alpha and hotspot,
into an input-transparent ARGB window. KWin includes that window in each
completed desktop frame, and XFixes hides the original sprite. Pointer events
continue to reach the underlying applications. Cursor shape changes, pointer
movement and compositor availability are monitored. Closing or killing the
helper releases its XFixes cursor hiding and restores the native pointer.

The Plasma service uses `--plasma` to query the session's
`org.freedesktop.ScreenSaver.GetActive` state. It restores the native cursor
while locked, or whenever lock state cannot be read. KWin correctly keeps
ordinary client windows behind the screen locker, so leaving the native
cursor hidden while locked would make the pointer disappear. Lock checks
run every 250 ms, with a 100 ms D-Bus method timeout.

## Build and install

Build on the target architecture with X11, XFixes, XRender, Xext and libsystemd
development packages and pkg-config installed:

```sh
make
sudo make install
install -D -m 644 neo-composited-cursor.service \
  "$HOME/.config/systemd/user/neo-composited-cursor.service"
systemctl --user daemon-reload
systemctl --user enable --now neo-composited-cursor.service
```

The user service manager must receive `DISPLAY`, `XAUTHORITY` and
`XDG_SESSION_TYPE=x11` from the Plasma session. The qualified Neo session
wrapper already imports these. No GPU or DCP driver reload is needed.

To restore the normal software cursor:

```sh
systemctl --user disable --now neo-composited-cursor.service
```

## Validation

`make check` requires Python 3, Xvfb, xcompmgr, xdpyinfo and xwininfo. It checks
compositor availability, singleton ownership, a viewable ARGB window with
nontransparent pixels and an empty input region, cursor movement, compositor
loss and recovery, normal exit, forced exit, and unavailable lock-state service.
The plain executable mode is for generic X11 testing; use `--plasma` for KDE.

On 2026-09-11, Neo J700 ran kernel `7.1.6-aurora-neo-gpu1`, KWin X11 6.7.5,
Xorg 21.1.24 and Mesa `j700` merge `c8d98bcc1000` (DJ `agx3` through
`ba7aee4190b`). With a stationary pointer over 800x600 gears, five-second
scanout sampling showed 519/2248 samples with fewer than half the bright
cursor pixels before the helper. All 2254 samples with the helper matched
every bright cursor pixel. Cursor position and scanout framebuffer ID stayed
constant during both runs. The user also confirmed the physical desktop was
visible and the moving pointer was steady over gears.

This is a userspace workaround for the X11 software sprite path. It does not
add a hardware cursor plane, fix DCP DPMS resume, or qualify Wayland and all
Plasma effects. The qualified desktop keeps Qt Quick shell rendering on
software while KWin and gears use Zink/Honeykrisp on the GPU. The helper uses
an 8 ms pointer poll; compositor latency still applies to its movement.

Host artifacts, runtime hashes, scanout samples and captures are kept in the
private workspace at `neo-linux/artifacts/agx3-ba7aee4-20260911/`.

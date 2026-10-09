# Visible J700 sleep/resume test

`screen-sleep.c` keeps the same SDL/GLES process, context, linked shader and
vertex buffer alive while an external controller requests sleep. It does not
suspend the machine itself. Build on Neo with:

```
cc -O2 -Wall -Wextra -Werror screen-sleep.c -o screen-sleep \
  $(pkg-config --cflags --libs sdl2 glesv2)
```

Run in the existing Wayland session with `GALLIUM_DRIVER=zink` and
`MESA_LOADER_DRIVER_OVERRIDE=zink`. Its two arguments are a phase file and a
writable capture directory. Phase 0 displays BEFORE SLEEP; 1 ENTERING SLEEP;
2 WOKEN UP! :D; 3 exits; 4 displays TEST STOPPED. Escape closes the window.
The caller owns the phase file. A renderer acknowledgement is written after
each buffer swap and includes the process ID, context pointer and monotonic
timestamp. This is a submission acknowledgement, not a physical panel timestamp.

Display phase 0 before starting a watched test and wait for the user's explicit
readiness response; a countdown is not a substitute for that response.
Autonomous power measurements require authorization in the current session.
With the qualified RTC/s2idle configuration, run:

```
python3 sleep-test.py --wake-after 60 --screen-directory CONTROL_DIRECTORY \
  --output NEW_RESULT_DIRECTORY
```

The control directory contains `phase` and `capture/`. After preparation, the
helper shows ENTERING SLEEP, holds it for **2 seconds**, and requests s2idle.
The RTC alarm includes this prelude so it does not consume the intended sleep
interval. After a successful timed return it requests WOKEN UP! :D and checks
that the renderer has the original PID and context. It never initiates another
cycle automatically. Inspect compositor screenshots to validate the visible
frames; the hardware panel's power state needs a separate observation.

## First watched result (earlier 0.5-second prelude)

On the development boot `c96d75a7-fc5d-41d5-8a2e-fd0fd584e962`, PID 2105
and GLES context `0xaaaafa353b20` survived. ENTERING SLEEP was held for
0.500112 seconds before the suspend request. The request returned after
60.020089 seconds boottime / 0.627950 seconds monotonic, about 59.392139
seconds of suspended timekeeping. All six CPUs entered s2idle and the PMU
alarm woke the system. The user observed that the physical display stayed on
throughout: display power-down remains unimplemented.

Zink reported Apple A18 Pro GPU (MESA_HONEYKRISP), Mesa 26.1.4
`git-5f07e8677e`. Independent compositor captures contained the expected cyan
triangle before sleep (RGB 26,204,255) and magenta triangle after wake
(242,77,191), with the requested text. The application's own `glReadPixels`
returned zero data both before and after sleep despite correct visible output
and no GL error. Retain that discrepancy as a separate baseline issue; the raw
PPM files are not proof of the visible rendering result. Evidence lives under
workspace `artifacts/neo-suspend/screen-sleep/` and on Neo in
`/tmp/neo-screen-sleep-01/` and `/root/dirtyroom-gpu/neo-suspend/screen-suspend-01/`.

The user subsequently requested the 2-second prelude. It is installed and
syntax-checked; the watched hardware result above used 0.5 seconds.

The later combined LCD/backlight PM hook is described in `display-power.md`.
Add `--display-off` to require and verify that hook during a visual sleep test.

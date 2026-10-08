# Surfaceless target cannot execute the official default GL3.0 procedure

Two full attempts (baseline and baseline-strace), same healthy boot and native Mesa hash, terminated before the main cases. Configuration metadata passed 15/15, and the no-error context case reported NotSupported because its required surface is a window. These are incomplete runs, not GL3.0 pass evidence.

The complete runner log contains `FATAL ERROR: surfaceless platform does not support --deqp-surface-type=window`. Standard-error buffering placed this line near the start of the combined log rather than at its tail. The syscall trace independently confirms that cts-runner calls `tgkill` on itself with SIGKILL. Upstream `framework/qphelper/qpDebugOut.c:225-242` implements this fatal-exit path. No kernel GPU fault, kernel-stream overrun, or cgroup memory kill was found.

The initial six-session inventory is valid only for the surfaceless target's fallback default configuration. It is superseded for acceptance by the forthcoming unmodified upstream `x11_egl` target. Do not change official test arguments to force surfaceless or count window-context NotSupported as implemented support. Regenerate the entire official inventory under X11/EGL and execute all required configurations there.

Tagged CTS source and test expectations remain unmodified. The X11 build uses a separate build directory and artifact prefix. Native Asahi remains the CTS client renderer; verify and record it before execution.

#!/bin/sh
export XDG_SESSION_TYPE=x11 XDG_CURRENT_DESKTOP=XFCE
export GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb
export LIBGL_ALWAYS_SOFTWARE=1
exec dbus-run-session -- /opt/j873/xfce-session.sh

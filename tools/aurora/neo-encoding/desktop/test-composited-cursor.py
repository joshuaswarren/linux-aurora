#!/usr/bin/env python3
"""Exercise the cursor's X11 lifecycle and empty input shape under Xvfb."""
import ctypes as C
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

binary = Path(sys.argv[1]).resolve()
display_name = ':191'
assert not Path('/tmp/.X11-unix/X191').exists(), 'test display is already in use'
env = dict(os.environ, DISPLAY=display_name)
env.pop('XAUTHORITY', None)
processes = []


def wait_until(predicate, description):
    end = time.monotonic() + 5
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(0.04)
    raise AssertionError(description)


with tempfile.TemporaryDirectory(prefix='neo-cursor-test-') as directory:
    log_path = Path(directory) / 'cursor.log'
    with log_path.open('w+') as log, open(os.devnull, 'w') as null:
        xvfb = subprocess.Popen(['Xvfb', display_name, '-screen', '0', '640x480x24',
                                 '-nolisten', 'tcp'], stdout=null, stderr=null)
        processes.append(xvfb)
        try:
            wait_until(lambda: subprocess.run(['xdpyinfo'], env=env, stdout=null,
                                                stderr=null).returncode == 0,
                       'Xvfb did not start')
            x = C.CDLL('libX11.so.6')
            shape = C.CDLL('libXext.so.6')
            x.XOpenDisplay.argtypes = [C.c_char_p]
            x.XOpenDisplay.restype = C.c_void_p
            x.XCloseDisplay.argtypes = [C.c_void_p]
            x.XDefaultRootWindow.argtypes = [C.c_void_p]
            x.XDefaultRootWindow.restype = C.c_ulong
            x.XInternAtom.argtypes = [C.c_void_p, C.c_char_p, C.c_int]
            x.XInternAtom.restype = C.c_ulong
            x.XGetSelectionOwner.argtypes = [C.c_void_p, C.c_ulong]
            x.XGetSelectionOwner.restype = C.c_ulong
            x.XCreateFontCursor.argtypes = [C.c_void_p, C.c_uint]
            x.XCreateFontCursor.restype = C.c_ulong
            x.XDefineCursor.argtypes = [C.c_void_p, C.c_ulong, C.c_ulong]
            x.XFreeCursor.argtypes = [C.c_void_p, C.c_ulong]
            x.XWarpPointer.argtypes = [C.c_void_p, C.c_ulong, C.c_ulong,
                                       C.c_int, C.c_int, C.c_uint, C.c_uint,
                                       C.c_int, C.c_int]
            x.XFlush.argtypes = [C.c_void_p]
            x.XFree.argtypes = [C.c_void_p]
            x.XGetImage.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_int,
                                   C.c_uint, C.c_uint, C.c_ulong, C.c_int]
            x.XGetImage.restype = C.c_void_p
            x.XGetPixel.argtypes = [C.c_void_p, C.c_int, C.c_int]
            x.XGetPixel.restype = C.c_ulong
            x.XDestroyImage.argtypes = [C.c_void_p]
            shape.XShapeGetRectangles.argtypes = [C.c_void_p, C.c_ulong, C.c_int,
                                                   C.POINTER(C.c_int), C.POINTER(C.c_int)]
            shape.XShapeGetRectangles.restype = C.c_void_p
            d = x.XOpenDisplay(display_name.encode())
            assert d
            root = x.XDefaultRootWindow(d)
            selection = x.XInternAtom(d, b'_NEO_COMPOSITED_CURSOR_S0', 0)
            cursor = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=log)
            processes.append(cursor)
            wait_until(lambda: x.XGetSelectionOwner(d, selection) != 0,
                       'cursor did not claim ownership')
            assert 'part of the composited frame' not in log_path.read_text()
            duplicate = subprocess.run([str(binary)], env=env, capture_output=True,
                                         text=True, timeout=3)
            assert duplicate.returncode == 1 and 'already owns' in duplicate.stderr
            print('PASS: waits for compositor and rejects duplicate instances')

            compositor = subprocess.Popen(['xcompmgr', '-a'], env=env,
                                            stdout=null, stderr=null)
            processes.append(compositor)
            wait_until(lambda: 'part of the composited frame' in log_path.read_text(),
                       'cursor did not activate with compositor')
            window = x.XGetSelectionOwner(d, selection)
            details = subprocess.check_output(['xwininfo', '-id', hex(window)], env=env, text=True)
            assert 'Depth: 32' in details and 'IsViewable' in details
            assert 'Override Redirect State: yes' in details
            width = int(re.search(r'Width:\s+(\d+)', details)[1])
            height = int(re.search(r'Height:\s+(\d+)', details)[1])
            picture = x.XGetImage(d, window, 0, 0, width, height, C.c_ulong(-1).value, 2)
            assert picture
            assert any(x.XGetPixel(picture, px, py) >> 24 for py in range(height) for px in range(width))
            x.XDestroyImage(picture)
            initial_position = tuple(int(re.search(r'Absolute upper-left ' + axis + r':\s+(-?\d+)', details)[1])
                                     for axis in ('X', 'Y'))
            count, order = C.c_int(-1), C.c_int(-1)
            rectangles = shape.XShapeGetRectangles(d, window, 2, C.byref(count), C.byref(order))
            assert count.value == 0, 'overlay intercepts pointer input'
            if rectangles:
                x.XFree(rectangles)
            print('PASS: ARGB overlay is visible and its input region is empty')

            hand = x.XCreateFontCursor(d, 60)  # XC_hand2
            x.XDefineCursor(d, root, hand)
            x.XWarpPointer(d, 0, root, 0, 0, 0, 0, 120, 140)
            x.XFlush(d)
            time.sleep(0.15)
            assert cursor.poll() is None
            details = subprocess.check_output(['xwininfo', '-id', hex(window)], env=env, text=True)
            position = tuple(int(re.search(r'Absolute upper-left ' + axis + r':\s+(-?\d+)', details)[1])
                             for axis in ('X', 'Y'))
            assert position != initial_position
            assert 120 - 64 <= position[0] <= 120 and 140 - 64 <= position[1] <= 140
            print('PASS: cursor shape and position changes remain live')

            compositor.terminate()
            compositor.wait(timeout=3)
            wait_until(lambda: 'Native cursor restored: compositor unavailable' in log_path.read_text(),
                       'native cursor did not recover after compositor exit')
            details = subprocess.check_output(['xwininfo', '-id', hex(window)], env=env, text=True)
            assert 'IsUnMapped' in details
            print('PASS: compositor exit unmaps overlay and restores native cursor')

            compositor = subprocess.Popen(['xcompmgr', '-a'], env=env,
                                            stdout=null, stderr=null)
            processes.append(compositor)
            wait_until(lambda: log_path.read_text().count('part of the composited frame') == 2,
                       'cursor did not recover with restarted compositor')
            cursor.terminate()
            assert cursor.wait(timeout=3) == 0
            wait_until(lambda: x.XGetSelectionOwner(d, selection) == 0,
                       'cursor selection leaked after exit')
            print('PASS: graceful exit releases display ownership')

            cursor = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=log)
            processes.append(cursor)
            wait_until(lambda: x.XGetSelectionOwner(d, selection) != 0,
                       'cursor could not restart')
            cursor.kill()
            cursor.wait(timeout=3)
            wait_until(lambda: x.XGetSelectionOwner(d, selection) == 0,
                       'client resources survived forced exit')
            print('PASS: forced exit releases client resources')

            previous_activations = log_path.read_text().count('part of the composited frame')
            protected = subprocess.Popen([str(binary), '--plasma'],
                env=dict(env, DBUS_SESSION_BUS_ADDRESS='unix:path=/nonexistent/neo-test-bus'),
                stdout=log, stderr=log)
            processes.append(protected)
            wait_until(lambda: x.XGetSelectionOwner(d, selection) != 0,
                       'protected cursor did not start')
            time.sleep(0.4)
            assert log_path.read_text().count('part of the composited frame') == previous_activations
            details = subprocess.check_output(['xwininfo', '-id', hex(x.XGetSelectionOwner(d, selection))], env=env, text=True)
            assert 'IsUnMapped' in details
            protected.terminate()
            assert protected.wait(timeout=3) == 0
            print('PASS: unavailable lock-state service leaves the native cursor in control')
            x.XFreeCursor(d, hand)
            x.XCloseDisplay(d)
        finally:
            for process in reversed(processes):
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
            print(log_path.read_text())

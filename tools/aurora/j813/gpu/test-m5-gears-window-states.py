#!/usr/bin/env python3
"""Exercise real XFCE maximize/fullscreen transitions and capture the client."""
import ctypes as C
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import struct
import sys
import tarfile
import time
import zlib

spec = importlib.util.spec_from_file_location(
    'resize', Path(__file__).with_name('test-m5-gears-resize.py'))
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)
assert b'apple,j813\0' in Path('/proc/device-tree/compatible').read_bytes()
os.environ['DISPLAY'] = ':0'
pid = int(Path('/run/m5-gears-first-frame.pid').read_text())
assert b'glxgears' in Path(f'/proc/{pid}/cmdline').read_bytes()
windows = r.command('xdotool', 'search', '--pid', str(pid), '--name', 'glxgears').split()
assert len(windows) == 1
window = int(windows[0])
out = Path(sys.argv[1])
out.mkdir(exist_ok=False)

class ClientMessage(C.Structure):
    _fields_ = [('type', C.c_int), ('serial', C.c_ulong), ('send_event', C.c_int),
                ('display', C.c_void_p), ('window', C.c_ulong),
                ('message_type', C.c_ulong), ('format', C.c_int),
                ('data', C.c_long * 5)]

class Event(C.Union):
    _fields_ = [('client', ClientMessage), ('padding', C.c_long * 24)]

x = C.CDLL('libX11.so.6')
x.XOpenDisplay.argtypes = [C.c_char_p]
x.XOpenDisplay.restype = C.c_void_p
x.XDefaultRootWindow.argtypes = [C.c_void_p]
x.XDefaultRootWindow.restype = C.c_ulong
x.XInternAtom.argtypes = [C.c_void_p, C.c_char_p, C.c_int]
x.XInternAtom.restype = C.c_ulong
x.XSendEvent.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_long, C.POINTER(Event)]
x.XFlush.argtypes = x.XCloseDisplay.argtypes = [C.c_void_p]
x.XGetImage.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_int,
                       C.c_uint, C.c_uint, C.c_ulong, C.c_int]
x.XGetImage.restype = C.POINTER(r.XImage)
x.XDestroyImage.argtypes = [C.POINTER(r.XImage)]
d = x.XOpenDisplay(b':0')
assert d
root = x.XDefaultRootWindow(d)

def state(action, *names):
    e = Event()
    e.client.type, e.client.display, e.client.window = 33, d, window
    e.client.message_type = x.XInternAtom(d, b'_NET_WM_STATE', 0)
    e.client.format = 32
    e.client.data[0], e.client.data[3] = action, 2
    for i, name in enumerate(names):
        e.client.data[i+1] = x.XInternAtom(d, name.encode(), 0)
    assert x.XSendEvent(d, root, 0, (1 << 19) | (1 << 20), C.byref(e))
    x.XFlush(d)
    time.sleep(1)

def record(name):
    geometry = r.command('xdotool', 'getwindowgeometry', '--shell', str(window))
    g = dict(line.split('=', 1) for line in geometry.splitlines() if '=' in line)
    w, h = int(g['WIDTH']), int(g['HEIGHT'])
    properties = r.command('xprop','-id',str(window),'_NET_WM_STATE')
    if name == 'fullscreen':
        assert '_NET_WM_STATE_FULLSCREEN' in properties and (w,h) == (2560,1664), (g,properties)
    else:
        assert '_NET_WM_STATE_MAXIMIZED_VERT' in properties and '_NET_WM_STATE_MAXIMIZED_HORZ' in properties
        assert 2540 <= w <= 2560 and 1500 <= h <= 1664, g
    entry = dict(state=name, size=[w, h], properties=properties, before=r.progress(), frames=[])
    for i in range(3):
        time.sleep(0.4)
        image = x.XGetImage(d, window, 0, 0, w, h, C.c_ulong(-1), 2)
        assert image
        try:
            a = image.contents
            assert (a.bits_per_pixel, a.byte_order, a.red_mask, a.green_mask,
                    a.blue_mask) == (32, 0, 0xff0000, 0xff00, 0xff)
            raw = C.string_at(a.data, a.bytes_per_line * h)
            lines = bytearray()
            for y in range(h):
                row = raw[y*a.bytes_per_line:y*a.bytes_per_line+w*4]
                rgb = bytearray(w*3)
                rgb[0::3], rgb[1::3], rgb[2::3] = row[2::4], row[1::4], row[0::4]
                lines.extend(b'\0' + rgb)
            filename = f'{name}-{i}.png'
            png = b'\x89PNG\r\n\x1a\n' + r.chunk(b'IHDR', struct.pack('>2I5B',w,h,8,2,0,0,0))
            png += r.chunk(b'IDAT', zlib.compress(lines)) + r.chunk(b'IEND',b'')
            (out/filename).write_bytes(png)
            entry['frames'].append(dict(file=filename, pixel_sha256=hashlib.sha256(lines).hexdigest()))
        finally:
            x.XDestroyImage(image)
    entry['after'] = r.progress()
    assert entry['after'] > entry['before'] + 5, entry
    assert len({f['pixel_sha256'] for f in entry['frames']}) == 3, entry
    print('M5_WINDOW_STATE_PASS',name,w,h,entry['before'],entry['after'],flush=True)
    return entry

report = dict(source='Actual X11 GLX client captures and paired M5 GPU completions',states=[],success=False)
try:
    state(1,'_NET_WM_STATE_MAXIMIZED_VERT','_NET_WM_STATE_MAXIMIZED_HORZ')
    report['states'].append(record('maximized'))
    state(1,'_NET_WM_STATE_FULLSCREEN')
    report['states'].append(record('fullscreen'))
    state(0,'_NET_WM_STATE_FULLSCREEN')
    report['states'].append(record('restored-maximized'))
    report['success'] = True
finally:
    x.XCloseDisplay(d)
    (out/'manifest.json').write_text(json.dumps(report,indent=2))
    (out/'kernel.log').write_text(r.command('dmesg'))
    (out/'application.log').write_text(Path('/run/m5-gears-first-frame.log').read_text())
    with tarfile.open(str(out)+'.tar.gz','w:gz') as t:
        for f in sorted(out.iterdir()): t.add(f,arcname=f.name)

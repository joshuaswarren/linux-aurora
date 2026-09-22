#!/usr/bin/env python3
"""Upload checked standard DRM tools and query the M5's apple KMS device.

Uses the existing Linux console only after the boot/probe owner has exited.
No modeset is performed by this helper.
"""
import base64
import hashlib
from pathlib import Path
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / 'm1n1/proxyclient'))
from m1n1.proxy import Serial

device = '/dev/cu.kis-01100000-ch-0'
owners = subprocess.run(['lsof', '-t', device, device.replace('/cu.', '/tty.')],
                        capture_output=True, text=True)
if owners.stdout.strip():
    raise RuntimeError('M5 console already has an owner')
bundle = (root / 'artifacts/display/drm-tools.tar.gz').read_bytes()
assert len(bundle) < 4 * 1024 * 1024
digest = hashlib.sha256(bundle).hexdigest().encode()

with Serial(device, 115200, timeout=0.2) as port:
    def receive(marker, seconds):
        captured = bytearray()
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            data = port.read(65536)
            if data:
                captured.extend(data)
                if b'\n' + marker + b'\r\n' in captured:
                    return bytes(captured)
        sys.stdout.buffer.write(captured[-2048:])
        sys.stdout.flush()
        raise RuntimeError(f'M5 console did not return {marker!r}; preserve the session')

    port.write(b'if grep -q apple,j813 /proc/device-tree/compatible && '
               b'[ $(uname -r) = 7.1.6-aurora-j813-dcp-v1 ] && '
               b'[ -d /sys/module/appledrm ]; then echo M5_KMS_TARGET_OK; '
               b'sha256sum /tmp/m5-drm-tools.tar.gz 2>/dev/null; fi; '
               b'echo M5_KMS_GUARD_DONE\n')
    guard = receive(b'M5_KMS_GUARD_DONE', 15)
    assert b'\nM5_KMS_TARGET_OK\r\n' in guard, 'Target guard refused upload'
    if digest not in guard:
        encoded = base64.b64encode(bundle)
        # The shell reads a heredoc before executing the command list.
        # Complete stty separately, then acknowledge bounded batches so a
        # megabyte upload cannot outrun the target console's receive queue.
        port.write(b"stty -echo; : >/tmp/m5-drm-tools.tar.gz; "
                   b"printf '\\nM5_KMS_NOECHO\\n'\n")
        receive(b'M5_KMS_NOECHO', 15)
        for batch_offset in range(0, len(encoded), 32768):
            batch = encoded[batch_offset:batch_offset + 32768]
            port.write(b"base64 -d >>/tmp/m5-drm-tools.tar.gz <<'M5_DRM_PART_EOF'\n")
            for offset in range(0, len(batch), 768):
                port.write(batch[offset:offset + 768] + b'\n')
            # With echo disabled, the shell's continuation prompt has no
            # trailing newline. Put the marker on its own line explicitly.
            port.write(b"M5_DRM_PART_EOF\nprintf '\\nM5_KMS_CHUNK_DONE\\n'\n")
            receive(b'M5_KMS_CHUNK_DONE', 20)
            if batch_offset % (8 * 32768) == 0:
                print('M5_KMS_UPLOAD', batch_offset + len(batch), '/', len(encoded), flush=True)
        port.write(b'stty echo; sha256sum /tmp/m5-drm-tools.tar.gz; '
                   b'echo M5_KMS_UPLOAD_DONE\n')
        result = receive(b'M5_KMS_UPLOAD_DONE', 60)
        assert digest in result, 'DRM tools archive checksum mismatch'
    print('M5_KMS_TOOLS_ARCHIVE_VERIFIED', digest.decode(), flush=True)
    port.write(b'mkdir -p /tmp/m5-drm-tools; tar -xzf /tmp/m5-drm-tools.tar.gz '
               b'-C /tmp/m5-drm-tools; '
               b'if (cd /tmp/m5-drm-tools && sha256sum -c SHA256SUMS); then '
               b'timeout 30 /tmp/m5-drm-tools/lib/ld-linux-aarch64.so.1 '
               b'--library-path /tmp/m5-drm-tools/lib /tmp/m5-drm-tools/modetest '
               b'-M apple -c -p -e; echo M5_KMS_QUERY_RETURN=$?; fi; '
               b'echo M5_KMS_QUERY_DONE\n')
    result = receive(b'M5_KMS_QUERY_DONE', 45)
    sys.stdout.buffer.write(result)
    assert b'\nM5_KMS_QUERY_RETURN=0\r\n' in result, 'DRM query failed'
print('M5_KMS_QUERY_CAPTURED; mode enumeration does not prove presentation')

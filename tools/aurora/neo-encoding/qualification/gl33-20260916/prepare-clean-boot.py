#!/usr/bin/env python3
"""Prepare CTS after a clean qualified boot before starting any display session."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import struct
import sys
from cts_faults import FATAL_GPU_FAULT

boot = sys.argv[1]
expected_mesa = sys.argv[2] if len(sys.argv) > 2 else '9117b99aec8456a4aeed3130ab069205af98ad94cd4bf30fc44eece4b6b529da'
assert re.fullmatch(r'[0-9a-f]{64}', expected_mesa)
expected_module = sys.argv[3] if len(sys.argv) > 3 else '63e78d2575cf4e9c181808e7c93799a201fcb5c63d31cfe80b8f2779c2b3c8c3'
assert re.fullmatch(r'[0-9a-f]{64}', expected_module)
assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
assert not Path('/run/neo-gl30-cts-prepared').exists()
assert not Path('/run/neo-dcp-probe-attempted').exists()
assert not Path('/sys/module/appledrm').exists()
assert subprocess.run(['systemctl','is-active','--quiet','sddm']).returncode != 0
digest = lambda p: hashlib.sha256(Path(p).read_bytes()).hexdigest()
mesa = '/opt/neo-mesa-native-gl2-20260913/lib/libgallium-26.1.4.so'
assert digest(mesa) == expected_mesa
module = '/lib/modules/7.1.6-aurora-neo-gpu1/kernel/drivers/gpu/drm/asahi/asahi.ko'
assert digest(module) == expected_module
elf = Path(module).read_bytes()
assert elf[:6] == b'\x7fELF\x02\x01'
section_offset = struct.unpack_from('<Q', elf, 0x28)[0]
section_size, section_count, strings_index = struct.unpack_from('<HHH', elf, 0x3a)
sections = [struct.unpack_from('<IIQQQQIIQQ', elf, section_offset + i * section_size)
            for i in range(section_count)]
strings_section = sections[strings_index]
strings = elf[strings_section[4]:strings_section[4] + strings_section[5]]
build_note = next(elf[s[4]:s[4] + s[5]] for s in sections
                  if strings[s[0]:].split(b'\0')[0] == b'.note.gnu.build-id')
assert Path('/sys/module/asahi/notes/.note.gnu.build-id').read_bytes() == build_note, 'loaded Asahi module differs from the verified file'
cts = Path('/opt/neo-cts-opengl-4.6.8.1-x11')
assert (cts/'source-commit.txt').read_text().strip() == '067e8832315e79817ede1c4863804e440f5d1c80'
for name, entry in json.loads((cts/'package-manifest.json').read_text()).items():
    assert (cts/name).stat().st_size == entry['size'] and digest(cts/name) == entry['sha256'], name
before = subprocess.check_output(['dmesg'])
assert not FATAL_GPU_FAULT.search(before)
out = Path('/home/mcruz/gl30-cts-20260913')/boot
out.mkdir(exist_ok=False)
(out/'kernel-clean-boot.log').write_bytes(before)
(out/'runtime.json').write_text(json.dumps(dict(boot=boot, kernel=subprocess.check_output(['uname','-a'],text=True).strip(),
    mesa_sha256=digest(mesa), asahi_module_sha256=digest(module), asahi_build_note=build_note.hex(), cts_commit=(cts/'source-commit.txt').read_text().strip()),indent=2)+'\n')
Path('/run/neo-gl30-cts-prepared').write_text(boot+'\n')
Path('/run/neo-gpu1-graphics').touch()
Path('/run/neo-gpu-use-zink').touch()
print('CLEAN_BOOT_CTS_READY_FOR_DCP_PROBE')

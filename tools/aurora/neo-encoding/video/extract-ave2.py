#!/usr/bin/env python3
"""Extract the pinned AVE2 code/strings/symbol metadata without modifying the KC."""
import hashlib
import argparse
import json
from pathlib import Path
import struct
import sys
from m1n1.macho import MachO, MachOLoadCmdType

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('source', type=Path)
p.add_argument('output', type=Path)
p.add_argument('--driver', choices=['AppleAVE2', 'AppleProResHW', 'AppleJPEGDriver'], default='AppleAVE2')
a = p.parse_args()
source, output = a.source, a.output
bundle_id = 'com.apple.driver.' + a.driver
expected = 'db55e48ac9e9c0f0db8e0e6e4d17efbfe8c077676f9b5eac5dd0c5f7a5f035fd'
data = source.read_bytes()
if hashlib.sha256(data).hexdigest() != expected:
    raise SystemExit('Expected the verified J700 25G76 kernelcache')
output.mkdir(parents=True, exist_ok=False)
macho = MachO(data)
image = macho.subfiles[bundle_id]
segments, sections, symbols = [], [], []
for command in image.get_cmds(MachOLoadCmdType.SEGMENT_64):
    s = command.args
    segments.append({'name': s.segname, 'address': hex(s.vmaddr), 'virtual_size': s.vmsize,
                     'file_offset': s.fileoff, 'file_size': s.filesize})
    # Fileset segment/section offsets are relative to the entire KC, not the
    # embedded header. Avoid MachO.prepare_image/load_symbols subfile offsets.
    if s.segname in ('__TEXT', '__TEXT_EXEC', '__DATA_CONST', '__DATA'):
        (output / (s.segname + '.bin')).write_bytes(data[s.fileoff:s.fileoff+s.filesize])
    for section in s.sections:
        sections.append({'segment': section.segname, 'name': section.sectname,
                         'address': hex(section.addr), 'size': section.size, 'file_offset': section.offset})
for command in image.get_cmds(MachOLoadCmdType.SYMTAB):
    table = command.args
    strings = data[table.stroff:table.stroff+table.strsize]
    for index in range(table.nsyms):
        strx, kind, section, desc, address = struct.unpack_from('<IBBHQ', data, table.symoff+index*16)
        if strx >= len(strings):
            raise SystemExit('Invalid string offset in fileset symbol table')
        end = strings.find(b'\0', strx)
        if end < 0:
            raise SystemExit('Unterminated fileset symbol name')
        name = strings[strx:end].decode('utf-8', 'replace')
        symbols.append({'name': name, 'address': hex(address), 'type': kind, 'section': section})
manifest = {'source_sha256': expected, 'system_build': '25G76', 'image': bundle_id,
            'fileset_header_offset': image.off, 'segments': segments, 'sections': sections, 'files': {}}
(output/'symbols.json').write_text(json.dumps(symbols,indent=2)+'\n')
for file in sorted(output.iterdir()):
    manifest['files'][file.name] = {'bytes': file.stat().st_size, 'sha256': hashlib.sha256(file.read_bytes()).hexdigest()}
(output/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
print(f'MEDIA_DRIVER_EXTRACTED driver={a.driver} sections={len(sections)} symbols={len(symbols)}')

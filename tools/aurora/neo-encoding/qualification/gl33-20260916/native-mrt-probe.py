#!/usr/bin/env python3
"""Capture native G17P MRT compiler evidence without opening a GPU device."""
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys

BASE = Path(__file__).resolve().parent
OUT = BASE / 'native-mrt-witness'
SEED = BASE.parents[2] / 'mesa-j700/src/asahi/compiler/test/probes/g17p-native-rt0-eot.btnf.hex'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def sections(data):
    """Enumerate bounded Mach-O sections, including nested GPU objects."""
    start = 0
    while True:
        start = data.find(b'\xcf\xfa\xed\xfe', start)
        if start < 0:
            return
        if start + 32 > len(data):
            return
        ncmds, sizecmds = struct.unpack_from('<II', data, start + 16)
        pos = start + 32
        end = pos + sizecmds
        if ncmds > 4096 or end > len(data):
            start += 4
            continue
        for _ in range(ncmds):
            cmd, size = struct.unpack_from('<II', data, pos)
            if size < 8 or pos + size > end:
                break
            if cmd == 0x19 and size >= 72:
                count = struct.unpack_from('<I', data, pos + 64)[0]
                for j in range(min(count, (size - 72) // 80)):
                    entry = pos + 72 + 80 * j
                    name = data[entry:entry+16].split(b'\0')[0].decode('ascii')
                    segment = data[entry+16:entry+32].split(b'\0')[0].decode('ascii')
                    length, offset = struct.unpack_from('<QI', data, entry + 40)
                    begin = start + offset
                    if length and begin + length <= len(data):
                        yield start, segment, name, data[begin:begin+length]
            pos += size
        start += 4


def compile_capture(name, source, script, builtin=False):
    directory = OUT / name
    directory.mkdir(parents=True, exist_ok=False)
    commands = []
    if builtin:
        input_path = directory / 'input.btnf'
        input_path.write_bytes(script)
        inputs = []
    else:
        (directory / 'input.metal').write_text(source)
        input_path = directory / 'pipeline.mtlp-json'
        input_path.write_text(json.dumps(script, indent=2) + '\n')
        commands += [
            ['xcrun', 'metal', '-c', str(directory/'input.metal'), '-o', str(directory/'input.air')],
            ['xcrun', 'metallib', str(directory/'input.air'), '-o', str(directory/'input.metallib')],
        ]
        inputs = [str(directory/'input.metallib')]
    commands.append(['xcrun', 'metal-nt', '-arch', 'applegpu_g17p',
                     '-platform_version', 'macos', '27.0', '27.0',
                     '-N', str(input_path), *inputs, '-o', str(directory/'output.nat')])
    receipt = {'name': name, 'commands': commands, 'hardware_execution': False}
    for i, command in enumerate(commands):
        run = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
        (directory/f'command-{i}.log').write_bytes(run.stdout)
        if run.returncode:
            receipt['exit_code'] = run.returncode
            break
    else:
        raw = (directory/'output.nat').read_bytes()
        receipt.update(exit_code=0, native_sha256=sha(raw), sections=[])
        receipt['sections'] = extract_sections(directory, raw)
    (directory/'receipt.json').write_text(json.dumps(receipt, indent=2)+'\n')
    print(name, receipt['exit_code'], [(s['name'],s['size']) for s in receipt.get('sections',[])], flush=True)


def extract_sections(directory, raw):
    result = []
    for i, (offset, segment, name, contents) in enumerate(sections(raw)):
        if name != '__text' and not segment.startswith('__GPU'):
            continue
        filename = f'{offset:06x}-{i}-{segment}-{name}.bin'
        (directory/filename).write_bytes(contents)
        result.append({'object_offset': offset, 'segment': segment, 'name': name, 'file': filename,
                       'size': len(contents), 'sha256': sha(contents)})
    return result


def builtin_input(count, mask, separate=False, partition_ids=None):
    seed = bytes.fromhex(SEED.read_text())
    assert len(seed) == 123 and sha(seed) == '6806d787abddcd75594ab0be2acd6da29c2438a91d52a333071a277df918801a'
    data = bytearray(seed[:84])
    data[80] = mask
    data += struct.pack('<I', count) + b'\0' * (4*count)
    for index in range(count):
        vtable = len(data)
        data += seed[92:112]
        table = len(data)
        data += seed[112:123]
        data[table+4] = partition_ids[index] if partition_ids is not None else (index if separate else 0)
        struct.pack_into('<I', data, 88+4*index, table-(88+4*index))
        data += b'\0' * (-len(data) % 4)
    return bytes(data)


def main():
    OUT.mkdir(exist_ok=False)
    for count in (1, 2, 3, 4, 8):
        for sparse in ((False, True) if count == 8 else (False,)):
            targets = [i for i in range(count) if not (sparse and i == 1)]
            members = '\n'.join(f'float4 c{i} [[color({i})]];' for i in targets)
            values = '\n'.join(f'o.c{i} = colors[{i}];' for i in targets)
            source = '#include <metal_stdlib>\nusing namespace metal;\n'
            source += 'vertex float4 mrt_vertex(uint id [[vertex_id]]) { return float4(float2((id<<1)&2,id&2)*2.0-1.0,0.0,1.0); }\n'
            source += 'struct Outputs {\n'+members+'\n};\n'
            source += 'fragment Outputs mrt_fragment(constant float4 *colors [[buffer(0)]]) { Outputs o;\n'+values+'\nreturn o; }\n'
            pipeline = {'pipelines': {'render_pipelines': [{'vertex_function': 'mrt_vertex',
                         'fragment_function': 'mrt_fragment',
                         'color_attachments': [{'pixel_format':'RGBA8Unorm'} for _ in range(count)]}]}}
            compile_capture(f'fragment-{count}'+('-sparse' if sparse else ''), source, pipeline)
        for separate in ((False, True) if count > 1 else (False,)):
            compile_capture(f'eot-{count}'+('-partitions' if separate else '-shared'), None,
                            builtin_input(count, (1<<count)-1, separate), True)
    compile_capture('eot-8-sparse', None, builtin_input(8, 0xfd, False), True)


if __name__ == '__main__':
    if sys.argv[1:] == ['--extract-existing']:
        for directory in sorted(OUT.iterdir()):
            receipt = json.loads((directory/'receipt.json').read_text())
            receipt['name'] = directory.name
            if receipt['exit_code'] == 0:
                receipt['sections'] = extract_sections(directory, (directory/'output.nat').read_bytes())
            (directory/'receipt.json').write_text(json.dumps(receipt, indent=2)+'\n')
        print('Existing native objects extracted with segment names.')
    elif sys.argv[1:] == ['--shared-class-eots']:
        for count, mask in ((4, 0xf), (8, 0xff), (8, 0xfd)):
            compile_capture(f'eot-{count}-classes-mask{mask:02x}', None,
                            builtin_input(count, mask, partition_ids=[i if i < 3 else 0 for i in range(count)]), True)
    elif sys.argv[1:] == ['--remaining-dense-eots']:
        for count in (3, 5, 6, 7):
            mask = (1 << count) - 1
            compile_capture(f'eot-{count}-classes-mask{mask:02x}', None,
                            builtin_input(count, mask, partition_ids=[i if i < 3 else 0 for i in range(count)]), True)
    elif sys.argv[1:] == ['--remaining-physical-partition-zero-eots']:
        for count in (5, 6, 7):
            compile_capture(f'eot-{count}-physical-partition-zero', None,
                            builtin_input(count, (1 << count) - 1), True)
    elif sys.argv[1:] == ['--compact-sparse-class-eots']:
        for mask in (0x04, 0x05, 0x0a, 0xfc, 0xfd, 0xfe):
            targets = [i for i in range(8) if mask & (1 << i)]
            compile_capture(f'eot-compact-classes-source-mask{mask:02x}', None,
                            builtin_input(len(targets), (1 << len(targets)) - 1,
                                          partition_ids=[i if i < 3 else 0 for i in targets]), True)
    elif sys.argv[1:] == ['--sparse-class-eots']:
        for mask in (0x04, 0x05, 0x0a, 0xfc, 0xfe):
            compile_capture(f'eot-8-classes-mask{mask:02x}', None,
                            builtin_input(8, mask, partition_ids=[i if i < 3 else 0 for i in range(8)]), True)
    elif sys.argv[1:] == ['--sparse-class-fragments']:
        for mask in (0x04, 0x05, 0x0a, 0xfc, 0xfe):
            targets = [i for i in range(8) if mask & (1 << i)]
            members = '\n'.join(f'float4 c{i} [[color({i})]];' for i in targets)
            values = '\n'.join(f'o.c{i} = colors[{i}];' for i in targets)
            source = '#include <metal_stdlib>\nusing namespace metal;\n'
            source += 'vertex float4 mrt_vertex(uint id [[vertex_id]]) { return float4(float2((id<<1)&2,id&2)*2.0-1.0,0.0,1.0); }\n'
            source += 'struct Outputs {\n'+members+'\n};\n'
            source += 'fragment Outputs mrt_fragment(constant float4 *colors [[buffer(0)]]) { Outputs o;\n'+values+'\nreturn o; }\n'
            pipeline = {'pipelines': {'render_pipelines': [{'vertex_function': 'mrt_vertex',
                         'fragment_function': 'mrt_fragment',
                         'color_attachments': [{'pixel_format': 'RGBA8Unorm' if mask & (1 << i) else 'Invalid'} for i in range(8)]}]}}
            compile_capture(f'fragment-8-active-mask{mask:02x}', source, pipeline)
    elif sys.argv[1:] == ['--missing-attachment']:
        previous = OUT / 'fragment-8-sparse'
        source = (previous/'input.metal').read_text()
        pipeline = json.loads((previous/'pipeline.mtlp-json').read_text())
        pipeline['pipelines']['render_pipelines'][0]['color_attachments'][1] = {'pixel_format': 'Invalid'}
        compile_capture('fragment-8-missing-attachment1', source, pipeline)
    else:
        main()

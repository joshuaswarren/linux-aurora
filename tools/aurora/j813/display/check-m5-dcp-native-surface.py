#!/usr/bin/env python3
"""Compare the actual C surface builder with the private native A472 capture.

This validates a wire record, not hardware presentation. Native 0xaa bytes
are tracer poison and must become zero in our newly allocated record.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('trace', type=Path)
args = parser.parse_args()
root = next(candidate for parent in Path(__file__).resolve().parents
            for candidate in (parent, parent / 'linux')
            if (candidate / 'drivers/gpu/drm/apple/iomfb_template.c').is_file())
source = (root / 'drivers/gpu/drm/apple/iomfb_template.c').read_text()
start = source.index('static void dcp_fill_dfb_surface(')
opening = source.index('{', start)
depth = 1
end = opening + 1
while depth:
    depth += (source[end] == '{') - (source[end] == '}')
    end += 1
function = source[start:end]

records = []
for line in args.trace.read_text().splitlines():
    fields = line.split()
    if len(fields) == 10 and fields[1] == 'CALL' and fields[6] == 'A472':
        assert fields[7:9] == ['0x230', '0x4']
        records.append(bytes.fromhex(fields[9]))
assert len(records) == 1, 'Expected the single native startup surface registration'
native = records[0]
assert len(native) == 0x230
assert native[0xb:0xf] == b'8a3b', 'Expected the captured H17G b3a8 surface'

harness = '''#include <stdint.h>
#include <stdio.h>
#include <string.h>
typedef uint8_t u8;
enum { DCP_IOMFB_METHODS_DEFAULT, DCP_IOMFB_METHODS_H17G };
struct apple_dcp { struct { int iomfb_method_profile; } hw; };
''' + function + '''
int main(void) {
    struct apple_dcp dcp = { .hw.iomfb_method_profile = DCP_IOMFB_METHODS_H17G };
    u8 request[0x230] = {0};
    dcp_fill_dfb_surface(&dcp, request);
    return fwrite(request, 1, sizeof(request), stdout) != sizeof(request);
}
'''
with tempfile.TemporaryDirectory(prefix='m5-dcp-surface-') as directory:
    path = Path(directory)
    (path / 'check.c').write_text(harness)
    subprocess.run(['clang', '-std=c11', '-Wall', '-Wextra', '-Werror',
                    str(path / 'check.c'), '-o', str(path / 'check')], check=True)
    actual = subprocess.check_output([str(path / 'check')])
assert len(actual) == len(native)
differences = [(offset, observed, expected)
               for offset, (observed, expected) in enumerate(zip(actual, native))
               if observed != (0 if expected == 0xaa else expected)]
assert not differences, f'Surface differs from native capture: {differences}'
print(f'H17G surface builder matches all {len(actual)} native bytes after poison removal')

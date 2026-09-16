#!/usr/bin/env python3
"""Exercise the exact Gallium rejection helper in isolated builder subprocesses."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[3]
source = ROOT / 'mesa-j700-preamble-20260915/src/gallium/drivers/asahi/agx_state.c'
match = re.search(
    r'static (?:NORETURN )?void\nagx_native_descriptor_rejected\([^\n]+\)\n\{.*?\n\}',
    source.read_text(), re.S)
assert match, 'production rejection helper not found'
helper = match.group()
program = '''
#include <stdlib.h>
#include "util/macros.h"
struct agx_device { unsigned chip; };
static const char *agx_chip_name(unsigned chip) { (void)chip; return "test"; }
#define mesa_loge(...) do { fprintf(stderr, __VA_ARGS__); fputc('\\n', stderr); } while (0)
''' + helper + '''
int main(void)
{
   struct agx_device dev = {0};
   agx_native_descriptor_rejected(&dev, "rejection regression");
   return 77; /* Any return instead of SIGABRT must fail the regression. */
}
'''
remote = '''
import json, pathlib, resource, signal, subprocess, tempfile
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
def check(program):
    with tempfile.TemporaryDirectory(prefix='agx-rejection-') as directory:
        p = pathlib.Path(directory)
        (p/'check.c').write_text(program)
        for release in (False, True):
            flags = ['-DNDEBUG'] if release else []
            subprocess.run(['cc', '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror',
                '-DHAVE_FUNC_ATTRIBUTE_NORETURN', *flags,
                '-I/home/mcruz.guest/neo/sources/mesa-native-gl2-20260913/src',
                str(p/'check.c'), '-o', str(p/'check')], check=True)
            result = subprocess.run([str(p/'check')], capture_output=True, text=True)
            assert result.returncode == -signal.SIGABRT, (release, result.returncode)
            assert 'rejection regression has no proven native descriptor encoding' in result.stderr
            print(json.dumps({'mode': 'release' if release else 'debug',
                              'termination': 'SIGABRT', 'diagnostic': True}))
'''
result = subprocess.run(['limactl', 'shell', 'neo-builder', 'python3', '-'],
    input=remote + '\ncheck(' + repr(program) + ')\n', text=True,
    capture_output=True, check=True)
print('production_helper_sha256', hashlib.sha256(helper.encode()).hexdigest())
print(result.stdout, end='')

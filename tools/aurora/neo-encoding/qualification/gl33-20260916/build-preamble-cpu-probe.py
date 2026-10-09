#!/usr/bin/env python3
"""Compare compiler preamble metadata without touching the Neo or its runtime."""
import difflib
import hashlib
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
REL = 'src/asahi/compiler/test/test-agx3-compile-fail-closed.c'
SOURCE = ROOT / 'mesa-j700-gs-cache-20260915' / REL
VM_SOURCE = '/home/mcruz.guest/neo/sources/mesa-native-gl2-20260913/' + REL
VM_BUILD = '/home/mcruz.guest/neo/build/mesa-native-gl2-20260913'
TARGET = 'src/asahi/compiler/agx3_compile_fail_closed_test'

def vm(*args, **kwargs):
    return subprocess.run(['limactl', 'shell', 'neo-builder', *args], check=True, **kwargs)

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def vm_sha(path):
    return vm('sha256sum', path, capture_output=True, text=True).stdout.split()[0]

def copy(path):
    subprocess.run(['limactl', 'copy', str(path), 'neo-builder:' + VM_SOURCE], check=True)
    vm('touch', VM_SOURCE)

def build(log):
    with (HERE / log).open('wb') as output:
        vm('ninja', '-C', VM_BUILD, TARGET, stdout=output, stderr=subprocess.STDOUT)

assert vm_sha(VM_SOURCE) == sha(SOURCE)
# A pre-existing test executable may have been linked against earlier compiler
# objects. Establish its current-source baseline before comparing restoration.
vm('touch', VM_SOURCE)
build('preamble-cpu-baseline-build.log')
original_binary = vm_sha(VM_BUILD + '/' + TARGET)
overlay = HERE / 'probes/preamble-cpu-overlay.c'
assert not overlay.exists()
text = SOURCE.read_text()
start = text.index('check_preamble_compute(enum agx_isa_encoding encoding)')
end = text.index('/*\n * hk_nir_lower_descriptors.c', start)
body = text[start:end]
body = body.replace('check_preamble_compute(enum agx_isa_encoding encoding)',
                    'check_preamble_compute(enum agx_isa_encoding encoding, bool enable)')
body = body.replace('   key.dev.encoding = encoding;',
                    '   key.dev.encoding = encoding;\n   key.no_preamble = !enable;')
a = body.index('   const bool valid = encoding ==')
b = body.index('   if (!valid)', a)
body = body[:a] + '''   const bool valid = compiled && part.binary &&
                      part.info.has_preamble == enable;
   printf("{\\"encoding\\":%u,\\"preamble_enabled\\":%s,"
          "\\"compiled\\":%s,\\"has_preamble\\":%s,"
          "\\"binary_size\\":%u,\\"main_offset\\":%u,\\"main_size\\":%u,"
          "\\"preamble_offset\\":%u,\\"push_halfs\\":%u,"
          "\\"main_gprs\\":%u,\\"preamble_gprs\\":%u,"
          "\\"scratch\\":%u,\\"preamble_scratch\\":%u,\\"binary_hex\\":\\"",
          encoding, enable ? "true" : "false", compiled ? "true" : "false",
          part.info.has_preamble ? "true" : "false", part.info.binary_size,
          part.info.main_offset, part.info.main_size, part.info.preamble_offset,
          part.info.push_count, part.info.nr_gprs, part.info.nr_preamble_gprs,
          part.info.scratch_size, part.info.preamble_scratch_size);
   for (unsigned i = 0; i < part.info.binary_size; ++i)
      printf("%02x", ((const unsigned char *)part.binary)[i]);
   puts("\\"}");
''' + body[b:]
text = text[:start] + body + text[end:]
main = text.index('int\nmain(int argc, char **argv)')
text = text[:main] + '''int
main(void)
{
   glsl_type_singleton_init_or_ref();
   bool ok = check_preamble_compute(AGX_ISA_ENCODING_AGX2, false);
   ok &= check_preamble_compute(AGX_ISA_ENCODING_AGX2, true);
   ok &= check_preamble_compute(AGX_ISA_ENCODING_AGX3B, false);
   ok &= check_preamble_compute(AGX_ISA_ENCODING_AGX3B, true);
   glsl_type_singleton_decref();
   return ok ? 0 : 1;
}
'''
overlay.write_text(text)
(HERE / 'probes/preamble-cpu-overlay.patch').write_text(''.join(difflib.unified_diff(
    SOURCE.read_text().splitlines(True), text.splitlines(True),
    fromfile='normal/' + REL, tofile='probe/' + REL)))
try:
    copy(overlay)
    build('preamble-cpu-probe-build.log')
    with (HERE / 'preamble-cpu-probe-results.jsonl').open('wb') as output:
        with (HERE / 'preamble-cpu-probe-stderr.log').open('wb') as errors:
            vm(VM_BUILD + '/' + TARGET, stdout=output, stderr=errors)
finally:
    copy(SOURCE)
    build('preamble-cpu-probe-restore-build.log')
    assert vm_sha(VM_SOURCE) == sha(SOURCE)
    assert vm_sha(VM_BUILD + '/' + TARGET) == original_binary
    print('COMPILER_TEST_SOURCE_AND_BINARY_RESTORED', original_binary, flush=True)

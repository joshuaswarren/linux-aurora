#!/usr/bin/env python3
"""Compile the real private ioctl layout/request validator with sanitizers."""
from pathlib import Path
import runpy
import subprocess
import tempfile

here = Path(__file__).resolve().parent
extract = runpy.run_path(str(here / 'test-linux-work.py'))['extract']
driver = here.parents[2] / 'drivers/soc/apple'
header = (driver / 'apple-ane-user.h').read_text()
header = '\n'.join(line for line in header.splitlines() if not line.startswith('#include'))
code = r'''
#include <assert.h>
#include <stdint.h>
#include <errno.h>
#include <stdio.h>
typedef uint32_t __u32;
typedef uint8_t __u8;
typedef int32_t __s32;
#define __aligned_u64 uint64_t __attribute__((aligned(8)))
#define ANE_CASE_INPUT_BYTES 4096U
#define ANE_CASE_OUTPUT_BYTES 32768U
'''+header+'\n'+extract((driver / 'apple-ane-fw-test.c').read_text(), 'static int ane_user_request_valid(')+r'''
_Static_assert(sizeof(struct aurora_ane_run)==64,"compat run layout");
_Static_assert(sizeof(struct aurora_ane_info)==88,"compat info layout");
int main(void) {
    struct aurora_ane_run run={.abi=1,.input_bytes=4096,.output_bytes=32768,.input=4096,.output=8192};
    assert(ane_user_request_valid(&run)==0);
    for(unsigned i=0;i<11;i++) {
        struct aurora_ane_run bad=run;
        switch(i) {
        case 0:bad.abi=0;break;case 1:bad.flags=1;break;
        case 2:bad.input_bytes=4095;break;case 3:bad.output_bytes=32769;break;
        case 4:bad.input=0;break;case 5:bad.output=0;break;
        case 6:bad.sequence=1;break;case 7:bad.elapsed_ns=1;break;
        case 8:bad.reserved[0]=1;break;case 9:bad.reserved[1]=1;break;
        case 10:bad.output_bytes=0;break;
        }
        assert(ane_user_request_valid(&bad)==-EINVAL);
    }
    run.cookie=UINT64_MAX;assert(ane_user_request_valid(&run)==0);
    puts("ANE_USER_REQUEST_PASS: 32/64-bit layout, version, sizes, reserved fields, output initialization");
}
'''
with tempfile.TemporaryDirectory(prefix='ane-user-request-') as temporary:
    c, exe = Path(temporary)/'test.c',Path(temporary)/'test'
    c.write_text(code)
    subprocess.run(['clang','-Wall','-Wextra','-Werror','-fsanitize=address,undefined',str(c),'-o',str(exe)],check=True)
    subprocess.run([str(exe)],check=True)

#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Test the real kernel embedding/argmax helpers against native ANE fixtures."""
import argparse
from pathlib import Path
import runpy
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reference-root', type=Path, required=True)
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    transport = runpy.run_path(str(here / 'test-ipc-transport.py'))
    header = transport['HEADER'].with_name('apple-ane-tiny.h').read_text()
    shim = '\n'.join(line for line in transport['SHIM'].splitlines()
                     if 'static inline uint32_t' not in line and 'static inline uint64_t' not in line)
    code = shim + header + r'''
#include <stdlib.h>
static void load(const char *root, const char *name, void *dst, unsigned size) {
    char path[4096]; assert(snprintf(path,sizeof(path),"%s/%s",root,name)<(int)sizeof(path));
    FILE *f=fopen(path,"rb");assert(f);assert(fread(dst,1,size,f)==size);assert(fgetc(f)==EOF);fclose(f);
}
int main(int argc, char **argv) {
    assert(argc==2);
    u8 embeddings[65536], input[4096], fixture[4096], output[32768];
    u32 tokens[32]={1,403,407,261,378}, next=999, expected[]={432,383,286};
    load(argv[1],"series/embeddings.bin",embeddings,sizeof(embeddings));
    for(unsigned i=0;i<3;i++) {
        char path[100];
        assert(apple_ane_tiny_input(embeddings,sizeof(embeddings),tokens,5+i,input,sizeof(input))==0);
        snprintf(path,sizeof(path),"series/%u.bin",i);load(argv[1],path,fixture,sizeof(fixture));
        assert(!memcmp(input,fixture,sizeof(input)));
        snprintf(path,sizeof(path),"series-output-%u.bin",i);load(argv[1],path,output,sizeof(output));
        assert(apple_ane_tiny_argmax(output,sizeof(output),4+i,&next)==0 && next==expected[i]);
        tokens[5+i]=next;
    }
    // Invalid tokens must not partially overwrite a live input buffer.
    memcpy(fixture,input,sizeof(input));tokens[1]=512;
    assert(apple_ane_tiny_input(embeddings,sizeof(embeddings),tokens,8,input,sizeof(input))==-ERANGE);
    assert(!memcmp(input,fixture,sizeof(input)));
    tokens[1]=403;
    assert(apple_ane_tiny_input(embeddings,sizeof(embeddings)-1,tokens,8,input,sizeof(input))==-EINVAL);
    assert(apple_ane_tiny_input(embeddings,sizeof(embeddings),tokens,33,input,sizeof(input))==-EINVAL);
    assert(apple_ane_tiny_input(embeddings,sizeof(embeddings),tokens,0,input,sizeof(input))==-EINVAL);
    assert(apple_ane_tiny_argmax(output,sizeof(output)-1,0,&next)==-EINVAL);
    assert(apple_ane_tiny_argmax(output,sizeof(output),32,&next)==-EINVAL);
    // Sweep all 65536 binary16 bit patterns in 512-value windows. Native C
    // floating-point comparison is an independent oracle for integer ordering.
    for(unsigned start=0;start<65536;start+=512) {
        float best=0;unsigned winner=0;bool invalid=false;
        memset(output,0,sizeof(output));
        for(unsigned t=0;t<512;t++) {
            uint16_t bits=start+t;_Float16 half;memcpy(&half,&bits,2);
            memcpy(output+(t*32+11)*2,&bits,2);
            if((bits&0x7c00)==0x7c00) invalid=true;
            if(!t || (float)half>best) {best=(float)half;winner=t;}
        }
        int result=apple_ane_tiny_argmax(output,sizeof(output),11,&next);
        assert(invalid ? result==-EILSEQ : result==0 && next==winner);
    }
    memset(output,0,sizeof(output));uint16_t minuszero=0x8000;
    memcpy(output,&minuszero,2);
    assert(apple_ane_tiny_argmax(output,sizeof(output),0,&next)==0 && next==0);
    // Positive/negative and subnormal ordering in the same vocabulary.
    uint16_t bits[]={0xbc00,0x8001,0,1,0x3c00,0x7bff};
    for(unsigned i=0;i<6;i++)memcpy(output+i*64,&bits[i],2);
    assert(apple_ane_tiny_argmax(output,sizeof(output),0,&next)==0 && next==5);
    assert(!writes && !reads);
    puts("ANEMLL_HOST_C_TESTS_PASS: actual autoregressive embeddings, native logits, all binary16 patterns, invalid bounds");
}
'''
    with tempfile.TemporaryDirectory(prefix='ane-tiny-test-') as temporary:
        cfile, executable = Path(temporary) / 'test.c', Path(temporary) / 'test'
        cfile.write_text(code)
        subprocess.run(['clang', '-std=gnu11', '-Wall', '-Wextra', '-Werror',
                        '-fsanitize=address,undefined', '-g', str(cfile), '-o', str(executable)], check=True)
        subprocess.run([str(executable), str(args.reference_root.resolve())], check=True)


if __name__ == '__main__':
    main()

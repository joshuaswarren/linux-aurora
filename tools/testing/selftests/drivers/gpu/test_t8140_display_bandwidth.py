#!/usr/bin/env python3
"""Exercise actual D003/map-reg callbacks against qualified DT resources."""
from pathlib import Path
import argparse
import hashlib
import os
import json
import re
import subprocess
import tempfile

BASE = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parent / "fixtures"
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--receipt", type=Path)
parser.add_argument("--dtb", required=True, type=Path)
args = parser.parse_args()
TREE = Path('drivers/gpu/drm/apple')

def function(source, name):
    match = re.search(r'^static [^;{}]*\b' + re.escape(name) + r'\([^;{}]*\)\n\{', source, re.M)
    assert match, name
    end = match.end()
    depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[match.start():end]

prefix = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
typedef uint32_t u32;
typedef uint64_t u64;
typedef uint64_t dma_addr_t;
#define DCP_FW_VERSION(a,b,c) (((a)<<16)|((b)<<8)|(c))
#define DCP_FW_VER DCP_FW_VERSION(26,6,0)
#define DCP_FW_NAME(x) x
#define DCP_H17P_DISP0_REG_INDEX 5
#define DCP_H17P_DISP0_REG_BASE 0x302800000ULL
#define DCP_H17P_DISP0_REG_SIZE 0xbc000ULL
#define APPLE_DCP_LINK_CALL_D003 3
#define dev_info(...) ((void)0)
#define dev_warn(...) ((void)0)
#define trace_iomfb_callback(...) ((void)0)
struct device { bool *of_node; };
struct resource { u64 start, end; };
static u64 resource_size(struct resource *r) { return r->end-r->start+1; }
static bool of_device_is_compatible(bool *external, const char *s) {
    return *external && !strcmp(s,"apple,t8140-dcpext");
}
struct apple_dcp {
    struct device *dev;
    struct resource disp_bw_scratch_res, disp_bw_doorbell_res;
    u32 disp_bw_scratch_offset, nr_disp_registers;
    struct resource *disp_registers[7];
};
struct apple_dcp_h17p_rt_bw_request { u32 id; };
struct apple_dcp_h17p_rt_bw_reply { u64 scratch, clock; u32 status; };
struct apple_dcp_link_rpc_view { int call,input_size,output_size; void *input,*output; };
/* Wire encoding is unchanged; mock only this transport helper. */
static int apple_dcp_h17p_rt_bw_encode_reply(struct apple_dcp_link_rpc_view *v,u64 s,u64 c) {
    struct apple_dcp_h17p_rt_bw_reply *r=v->output;
    *r=(struct apple_dcp_h17p_rt_bw_reply){s,c,0}; return 0;
}
struct dcp_map_reg_req { u32 index; };
struct dcp_map_reg_resp { u64 dva,addr,length; u32 ret; };
'''

tests = r'''
int main(void) {
    bool external=true; struct device dev={&external};
    struct apple_dcp d={.dev=&dev,.nr_disp_registers=6,
      .disp_bw_scratch_res={BWR_BASE,BWR_BASE+7},
      .disp_bw_doorbell_res={BOOST_BASE,BOOST_BASE+3}};
    d.disp_registers[4]=&d.disp_bw_scratch_res;
    d.disp_registers[5]=&d.disp_bw_doorbell_res;
    struct apple_dcp_h17p_rt_bw_request in={0};
    struct apple_dcp_h17p_rt_bw_reply out={0};
    assert(trampoline_rt_bandwidth_h17p(&d,3,&out,&in));
    struct dcp_map_reg_req req={4};
    struct dcp_map_reg_resp bwr=dcpep_cb_map_reg(&d,&req);
    req.index=5; struct dcp_map_reg_resp boost=dcpep_cb_map_reg(&d,&req);
    /* Native firmware subtracts the descriptor's physical base in 32 bits. */
    u32 offset=(u32)out.scratch-(u32)bwr.addr;
    assert(offset<=bwr.length && bwr.length-offset>=8);
    assert(out.scratch==0x302828000ULL && out.clock==0x302870000ULL);
    assert(bwr.addr==out.scratch && bwr.dva==bwr.addr && bwr.length==8 && !bwr.ret);
    assert(boost.addr==out.clock && boost.dva==boost.addr && boost.length==4 && !boost.ret);
    req.index=7; assert(dcpep_cb_map_reg(&d,&req).ret==1);
    /* Preserve the qualified internal controller's existing workaround. */
    external=false;
    assert(trampoline_rt_bandwidth_h17p(&d,3,&out,&in));
    req.index=5; bwr=dcpep_cb_map_reg(&d,&req);
    assert(out.scratch==0x302820000ULL && out.clock==0x302868000ULL);
    assert(bwr.addr==0x302800000ULL && bwr.dva==bwr.addr && bwr.length==0xbc000);
    assert(out.scratch>=bwr.addr && out.scratch+8<=bwr.addr+bwr.length);
    puts("PASS: external D003 addresses fit exact BWR/boost descriptors; internal aperture and invalid index behavior preserved");
}
'''

dtb = args.dtb
cells = subprocess.check_output(['fdtget','-t','x',str(dtb),
    '/soc/display-bandwidth@302828000','reg'], text=True).split()
values = [int(x,16) for x in cells]
assert values[2:4] == [0,8] and values[6:8] == [0,4]
bwr = values[0] << 32 | values[1]
boost = values[4] << 32 | values[5]
prefix += f'\n#define BWR_BASE {bwr}ULL\n#define BOOST_BASE {boost}ULL\n'
source_hashes = {}
with tempfile.TemporaryDirectory() as directory:
    for label,root in [('candidate',BASE),('baseline',FIXTURES)]:
        funcs=[]
        for name,func in [('iomfb_h17p.c','trampoline_rt_bandwidth_h17p'),
                          ('iomfb_template.c','dcpep_cb_map_reg')]:
            p=(root/TREE/name) if label=='candidate' else root/('before-'+name)
            funcs.append(function(p.read_text(),func))
            if label=='candidate': source_hashes[name]=hashlib.sha256(p.read_bytes()).hexdigest()
        p=Path(directory)/f'{label}.c'; exe=p.with_suffix('')
        p.write_text(prefix+'\n'.join(funcs)+tests)
        subprocess.run([os.environ.get('CC', 'clang'),'-std=c11','-Wall','-Wextra',
                        '-Wno-unused-parameter','-fsanitize=address,undefined','-g',
                        str(p),'-o',str(exe)],check=True)
        r=subprocess.run([str(exe)],capture_output=True,text=True)
        if label=='candidate':
            assert r.returncode==0,r.stderr
            print(r.stdout.strip())
        else:
            assert r.returncode!=0 and 'assert' in r.stderr.lower(),r.stderr
            print('PASS: DP5 baseline fails the firmware descriptor-offset invariant')
receipt = {
    'candidate_passed':True,'baseline_regression_reproduced':True,
    'actual_callbacks':True,'wire_encoder_mocked':True,
    'sanitizers':['address','undefined'],'sources':source_hashes,
    'dtb_sha256':hashlib.sha256(dtb.read_bytes()).hexdigest(),
    'hardware_verified':False}
if args.receipt:
    args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')

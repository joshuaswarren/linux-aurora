#!/usr/bin/env python3
"""Run the actual PPR/version handshake against bounded response fixtures."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage

root = Path(__file__).resolve().parents[4]
source = local_linkage((root / 'drivers/net/wireless/mediatek/mt7932/mt7932.c').read_text())
function = 'static int mt_precal_gate(' + source.split('static int mt_precal_gate(', 1)[1].split(
    'static int mt_runtime_start(', 1)[0]
prefix = r'''
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <assert.h>
#include <string.h>
#include <errno.h>
typedef uint8_t u8;typedef uint32_t u32;
struct pci {int dev;};
struct mt7932 {bool runtime_epoch,runtime_cal_valid;u8 board_type,runtime_cal,smart_version,preload_version;
 u8 reply[128];size_t reply_length;struct pci *pdev;};
struct firmware {const u8 *data;size_t size;};
static unsigned calls;static int fault;static u8 data[412];
static struct firmware firmware={data,sizeof(data)};
static void dev_info(void *d,const char *fmt,...) {(void)d;(void)fmt;}
static u32 get_unaligned_le32(const void*p) {const u8*b=p;return b[0]|b[1]<<8|b[2]<<16|(u32)b[3]<<24;}
static int request_firmware_direct(const struct firmware **f,const char*n,void*d) {
 (void)d;assert(!strcmp(n,"mediatek/mt7932/ppr.bin"));*f=&firmware;return fault==1?-ENOENT:0;
}
static void release_firmware(const struct firmware*f) {assert(f==&firmware);}
static int mt_request_ext(struct mt7932*m,u8 cid,u8 ext,bool runtime,bool set,bool wait,const void*p,size_t n) {
 unsigned at=calls++;assert(cid==0xed && runtime && !set && wait);
 assert(at<2);memset(m->reply,0,sizeof(m->reply));m->reply[28]=0xed;
 if(!at) {
  assert(ext==0x21 && n==416 && !memcmp(p,"\2\0\234\1",4));
  assert(!memcmp((const u8*)p+4,data,412));m->reply_length=44;
  if(fault==2)m->reply_length=43;
  if(fault==3)m->reply[40]=1;
 } else {
  assert(ext==0xa9 && n==4 && !get_unaligned_le32(p));
  m->reply[32]=0xaa;m->reply[38]=12;m->reply_length=40;
  if(fault==4)m->reply[32]=0;
  if(fault==5)return -ETIMEDOUT;
 }
 return 0;
}
static int mt_request(struct mt7932*m,u8 cid,bool runtime,bool set,bool wait,const void*p,size_t n) {
 assert(calls++==2 && cid==0xbf && runtime && !set && wait && n==20);
 for(unsigned i=0;i<n;i++)assert(((const u8*)p)[i]==(i==3?9:0));
 memset(m->reply,0,sizeof(m->reply));m->reply_length=56;m->reply[28]=0xa5;
 m->reply[37]=m->reply[44]=1;m->reply[38]=2;m->reply[48]=0xbf;
 if(fault==6)m->reply_length=55;
 if(fault==7)m->reply[48]=0;
 if(fault==8)m->reply[37]=0;
 if(fault==9)m->reply[44]=2;
 return 0;
}
'''
tests = r'''
int main(void) {
 struct pci pci={0};
 for(fault=0;fault<13;fault++) {
  struct mt7932 m={.runtime_epoch=true,.runtime_cal_valid=true,.board_type=3,.pdev=&pci};
  calls=0;if(fault==10)m.runtime_cal=1;
  if(fault==11)m.runtime_cal_valid=false;
  if(fault==12)m.board_type=2;
  int ret=mt_precal_gate(&m);
  assert(fault?ret<0:ret==0);
  assert(m.preload_version==(fault?0:2));
 }
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-precal-') as tmp:
    c, binary = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + function + tests)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    '-g', str(c), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('MT7932 PPR/version handshake: 13 success/failure paths passed')

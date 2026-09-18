#!/usr/bin/env python3
"""Execute actual bounded UID/eFuse query gate; no hardware or firmware writes."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage

root = Path(__file__).resolve().parents[4]
source = local_linkage((root / 'drivers/net/wireless/mediatek/mt7932/mt7932.c').read_text())
function = 'static int mt_board_gate(' + source.split('static int mt_board_gate(', 1)[1].split(
    'static int mt_precal_gate(', 1)[0]
prefix = r'''
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <assert.h>
#include <string.h>
#include <errno.h>
typedef uint8_t u8;typedef uint32_t u32;
#define ARRAY_SIZE(a) (sizeof(a)/sizeof((a)[0]))
struct pci {int dev;};
struct mt7932 {bool runtime_epoch;u8 board_type,module_byte;u8 reply[128];size_t reply_length;struct pci *pdev;};
static unsigned calls;static int fault;
static void dev_info(void *d,const char *fmt,...) {(void)d;(void)fmt;}
static u32 get_unaligned_le32(const void*p) {const u8*b=p;return b[0]|b[1]<<8|b[2]<<16|(u32)b[3]<<24;}
static void put_unaligned_le32(u32 v,void*p) {u8*b=p;for(unsigned i=0;i<4;i++)b[i]=v>>(8*i);}
static int mt_request_ext(struct mt7932*m,u8 cid,u8 ext,bool runtime,bool set,bool wait,const void*p,size_t n) {
 unsigned at=calls++;assert(cid==0xed && runtime && !set && wait);
 assert(at<3);memset(m->reply,0,sizeof(m->reply));m->reply[28]=0xed;
 if(!at) {
  assert(ext==0xc1 && !p && !n);m->reply[32]=0xc1;m->reply_length=44;
  if(fault==1)m->reply_length=43;
  if(fault==2)m->reply[32]=0;
 } else {
  u32 addr=at==1 ? 0x70:0x160;assert(ext==1 && n==24 && get_unaligned_le32(p)==addr);
  for(unsigned i=4;i<24;i++)assert(!((const u8*)p)[i]);
  m->reply[32]=1;m->reply_length=60;put_unaligned_le32(addr,m->reply+36);
  put_unaligned_le32(1,m->reply+40);if(at==1)m->reply[54]=1;
  if(at==1 && fault==3)m->reply_length=59;
  if(at==1 && fault==4)put_unaligned_le32(0x80,m->reply+36);
  if(at==1 && fault==5)m->reply[28]=0xee;
  if(at==1 && fault==6)return -ETIMEDOUT;
 }
 return 0;
}
'''
tests = r'''
int main(void) {
 struct pci pci={0};struct mt7932 m={.runtime_epoch=true,.pdev=&pci};
 for(fault=0;fault<8;fault++) {
  calls=0;m.runtime_epoch=fault!=7;
  int ret=mt_board_gate(&m);
  assert(fault ? ret<0:ret==0);
  assert(calls==(fault==0 ? 3:fault==7 ? 0:fault<=2 ? 1:2));
 }
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-board-query-') as tmp:
    c, binary = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + function + tests)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    '-g', str(c), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('MT7932 board gate: read-only envelopes, bounds, event/address mismatch and timeout guards passed')

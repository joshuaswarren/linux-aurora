#!/usr/bin/env python3
"""Execute real runtime scheduler/ring rebuild against checked fake MMIO."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage

root = Path(__file__).resolve().parents[4]
source = local_linkage((root / 'drivers/net/wireless/mediatek/mt7932/recovery.c').read_text())
functions = 'static int mt_runtime_scheduler(' + source.split(
    'static int mt_runtime_scheduler(', 1)[1].split('static void mt_runtime_publish(', 1)[0]
prefix = r'''
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdbool.h>
#include <string.h>
#include <errno.h>
typedef uint8_t u8;typedef uint16_t u16;typedef uint32_t u32;typedef uint64_t dma_addr_t;
#define BIT(n) (1U<<(n))
#define ARRAY_SIZE(a) (sizeof(a)/sizeof((a)[0]))
#define U32_MAX UINT32_MAX
#define cpu_to_le32(x) ((u32)(x))
#define le32_to_cpu(x) ((u32)(x))
#define lower_32_bits(x) ((u32)(x))
#define upper_32_bits(x) ((u32)((x)>>32))
#define dma_wmb() ((void)0)
#define W 0xd4000
#define H 0xd6000
#define C 0xd7000
#define DONE BIT(31)
#define MT7932_RX_CAPACITY 0x930
#define RX_STRIDE 0x940
struct mt7932_desc {u32 address,control,second,info;};
struct mt7932_ring {struct mt7932_desc *desc;dma_addr_t dma,buffers_dma;u8 *buffers;
 u32 reg;u16 count,head,tail,queued,slots,used;};
struct mt7932 {struct mt7932_ring tx[19],rx[9];bool runtime_epoch;};
static u32 regs[0x100000/4],offsets[512],values[512];
static unsigned writes;static int fault;
static u32 mt_read(struct mt7932*m,u32 reg) {(void)m;assert(reg<0x100000 && !(reg&3));return regs[reg/4];}
static void mt_write(struct mt7932*m,u32 reg,u32 value) {
 (void)m;assert(reg<0x100000 && !(reg&3) && writes<512);
 assert(reg!=W+0x100 && reg!=W+0x2b0); /* no forced hardware/EXT reset */
 if(reg>=W+0x300 && reg<W+0x590)assert((reg&15)!=12); /* DIDX never written */
 offsets[writes]=reg;values[writes++]=value;regs[reg/4]=value;
 if(reg==W+0x20c) {
  assert(value==UINT32_MAX);
  for(unsigned i=0;i<19;i++)regs[(W+0x300+16*i+12)/4]=0;
  if(fault==5)regs[(W+0x300+16*17+12)/4]=3;
 }
}
static void mt_rmw(struct mt7932*m,u32 r,u32 mask,u32 v) {mt_write(m,r,(mt_read(m,r)&~mask)|v);}
static int mt_poll(struct mt7932*m,u32 r,u32 mask,u32 value,u32 timeout) {
 (void)timeout;if(fault==4)return -ETIMEDOUT;
 return (mt_read(m,r)&mask)==value ? 0:-ETIMEDOUT;
}
static void mt_tx_clean(struct mt7932_ring*q) {(void)q;} /* independently tested transport */
'''
tests = r'''
int main(void) {
 struct mt7932 m={0};const unsigned counts[9]={32,0,512,32,32,224,128,512,64};
 for(unsigned i=0;i<19;i++) {
  struct mt7932_ring*q=&m.tx[i];q->count=i==15?0:i<15?512:i==16?256:i==17?24:16;
  q->desc=calloc(q->count+1,sizeof(*q->desc));assert(q->desc);
  q->reg=W+0x300+16*i;q->dma=0x4000+0x4000*i;
 }
 for(unsigned i=0;i<9;i++) {
  struct mt7932_ring*q=&m.rx[i];q->count=counts[i];q->slots=q->count;
  q->desc=calloc(q->count+1,sizeof(*q->desc));assert(q->desc);
  q->buffers=(u8*)q->desc;q->buffers_dma=0x100000+0x200000*i;
  q->reg=W+0x500+16*i;q->dma=0x80000+0x4000*i;
 }
 for(fault=0;fault<7;fault++) {
  memset(regs,0,sizeof(regs));writes=0;m.runtime_epoch=false;
  regs[(W+0x208)/4]=0x1010b870;regs[(W+0x2b0)/4]=0x28c004df;
  for(unsigned i=0;i<19;i++) {
   m.tx[i].queued=0;m.tx[i].head=m.tx[i].tail=7;m.tx[i].used=20;
   for(unsigned j=0;j<m.tx[i].count;j++)m.tx[i].desc[j].control=DONE;
  }
  for(unsigned i=0;i<9;i++) {
   m.rx[i].slots=m.rx[i].count;
   for(unsigned j=0;j<m.rx[i].count;j++)m.rx[i].desc[j].control=DONE|BIT(30)|0x123;
  }
  if(fault==1)m.tx[17].queued=1;
  if(fault==2)m.rx[8].slots--;
  if(fault==3)regs[(W+0x208)/4]=0x50209045;
  if(fault==6)m.runtime_epoch=true;
  int ret=mt_runtime_rebuild(&m);
  if(fault) {
   assert(ret<0);
   if(fault==1 || fault==2 || fault==3 || fault==6) {
    assert(!writes && m.tx[17].desc[0].control==DONE);
    assert(m.rx[0].desc[0].control==(DONE|BIT(30)|0x123));
   }
   if(fault==4)assert(ret==-ETIMEDOUT && writes==1);
   if(fault==5)assert(!m.runtime_epoch && m.tx[17].used==20);
   continue;
  }
  assert(!ret && m.runtime_epoch);
  u32 published=0;
  for(unsigned k=0;k<writes;k++) {
   u32 r=offsets[k];
   if(r>=W+0x300 && r<=W+0x420 && !(r&15)) {
    unsigned i=(r-W-0x300)/16;assert(!(published&BIT(i)));published|=BIT(i);
    assert(values[k]==m.tx[i].dma && offsets[k+1]==r+8 && values[k+1]==0);
    assert(offsets[k+2]==r+4 && values[k+2]==m.tx[i].count);
   }
   if(r>=W+0x500 && r<=W+0x580 && !(r&15)) {
    unsigned i=(r-W-0x500)/16;
    assert(values[k]==m.rx[i].dma && offsets[k+1]==r+8 && values[k+1]==m.rx[i].count-1U);
    assert(offsets[k+2]==r+4 && values[k+2]==m.rx[i].count);
   }
  }
  assert(published==0x77fff);
  for(unsigned i=0;i<19;i++) {
   assert(!m.tx[i].used && !m.tx[i].head && !m.tx[i].tail);
   for(unsigned j=0;j<m.tx[i].count;j++)assert(!m.tx[i].desc[j].control);
  }
  for(unsigned i=0;i<9;i++) {
   assert(m.rx[i].queued==counts[i] && !m.rx[i].head && !m.rx[i].tail);
   for(unsigned j=0;j<m.rx[i].count;j++) {
    struct mt7932_desc*d=&m.rx[i].desc[j];
    assert(d->address==m.rx[i].buffers_dma+j*RX_STRIDE && !d->second && !d->info);
    assert(d->control==(BIT(30)|0x123|(MT7932_RX_CAPACITY<<16)));
   }
  }
  assert(mt_runtime_scheduler(&m,true)==0);
  assert(mt_runtime_scheduler(&m,false)==0);
  regs[(H+0x68)/4]^=1;assert(mt_runtime_scheduler(&m,false)==-EIO);
 }
 for(unsigned i=0;i<19;i++)free(m.tx[i].desc);
 for(unsigned i=0;i<9;i++)free(m.rx[i].desc);
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-runtime-') as tmp:
    c, binary = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + functions + tests)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    '-g', str(c), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('MT7932 runtime: real rebuild ownership, publication, geometry, timeout and reset guards passed')

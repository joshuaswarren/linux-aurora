#!/usr/bin/env python3
"""Exercise actual packet RX group parser and 35-bit Ethernet pointer builder."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[4]
header = root / 'drivers/net/wireless/mediatek/mt7932/packet.h'
source = '\n'.join(line for line in header.read_text().splitlines() if not line.startswith('#include'))
source = header.with_name('channels.h').read_text() + '\n' + source
prefix = r'''
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <assert.h>
#include <string.h>
#include <errno.h>
typedef uint8_t u8;typedef uint16_t u16;typedef uint32_t u32;typedef uint64_t u64;
static u16 get_unaligned_le16(const void*v) {const u8*p=v;return p[0]|p[1]<<8;}
static u16 get_unaligned_be16(const void*v) {const u8*p=v;return p[1]|p[0]<<8;}
static u32 get_unaligned_le32(const void*v) {const u8*p=v;return p[0]|p[1]<<8|p[2]<<16|(u32)p[3]<<24;}
static void put_unaligned_le16(unsigned v,void*out) {u8*p=out;p[0]=v;p[1]=v>>8;}
static void put_unaligned_le32(u32 v,void*out) {u8*p=out;for(int i=0;i<4;i++)p[i]=v>>(8*i);}
'''
tests = r'''
int main(void) {
 u8 packet[256],tx[64],ethernet[1514]={0};struct mt7932_rx_frame frame;
 const unsigned groups[]={4,1,2,3,5},sizes[]={16,16,8,8,72};
 for(unsigned mask=0;mask<32;mask++) {
  memset(packet,0,sizeof(packet));size_t offset=24;
  for(unsigned i=0;i<5;i++)if(mask&(1U<<(groups[i]-1))) {
   if(groups[i]==3)put_unaligned_le32(0x006465,packet+offset+4);
   if(groups[i]==5)put_unaligned_le32(0x006465,packet+offset+24);
   offset+=sizes[i];
  }
  packet[offset]=0x80;size_t length=offset+36;
  put_unaligned_le32((2U<<27)|length,packet);put_unaligned_le32(mask<<11,packet+4);
  put_unaligned_le32(6<<8,packet+12);
  int ret=mt7932_rx_frame(packet,length,&frame);
  if((mask&16) && !(mask&4)) {assert(ret<0);continue;}
  assert(!ret && frame.channel==6 && frame.length==36 && frame.data==packet+offset);
  assert(mt7932_rx_beacon(&frame));
  for(unsigned ch=36;ch<=48;ch+=4) {
   put_unaligned_le32(ch<<8,packet+12);
   assert(!mt7932_rx_frame(packet,length,&frame) && frame.channel==ch);
  }
  put_unaligned_le32(181<<8,packet+12);assert(mt7932_rx_frame(packet,length,&frame)<0);
  put_unaligned_le32(6<<8,packet+12);assert(!mt7932_rx_frame(packet,length,&frame));
  put_unaligned_le32(1U<<25,packet+8);
  assert(!mt7932_rx_frame(packet,length,&frame));
  put_unaligned_le32(1U<<23,packet+8);assert(mt7932_rx_frame(packet,length,&frame)<0);
  put_unaligned_le32(1U<<24,packet+8);assert(mt7932_rx_frame(packet,length,&frame)<0);
  put_unaligned_le32(0,packet+8);
  assert(mt7932_rx_signal(&frame,1)==((mask&4)?-60:-128));
  assert(mt7932_rx_signal(&frame,0)==-128);
  for(size_t cut=0;cut<length;cut++)assert(mt7932_rx_frame(packet,cut,&frame)<0);
  assert(!mt7932_rx_frame(packet,length,&frame));frame.length=37;
  assert(!mt7932_rx_beacon(&frame));
  put_unaligned_le32((mask<<11)|(1U<<25),packet+4);assert(mt7932_rx_frame(packet,length,&frame)<0);
 }
 memset(packet,0,sizeof(packet));put_unaligned_le32((8U<<27)|38,packet);
 put_unaligned_le32(1U<<13,packet+8);put_unaligned_le32(1<<8,packet+12);
 assert(!mt7932_rx_frame(packet,38,&frame) && frame.translated && frame.length==14 && !frame.group4);
 assert(!mt7932_rx_beacon(&frame));
 put_unaligned_le32((1U<<13)|(1U<<27),packet+8);assert(mt7932_rx_frame(packet,38,&frame)<0);
 /* Saved group4 More Fragments is independent of RXD bit27. */
 put_unaligned_le32((8U<<27)|54,packet);put_unaligned_le32(1U<<14,packet+4);
 put_unaligned_le32(1U<<13,packet+8);put_unaligned_le16(0x88,packet+24);
 assert(!mt7932_rx_frame(packet,54,&frame)&&frame.group4&&frame.length==14);
 put_unaligned_le16(0x488,packet+24);
 assert(mt7932_rx_frame(packet,54,&frame)==-EOPNOTSUPP);
 put_unaligned_le32((8U<<27)|38,packet);put_unaligned_le32(0,packet+4);
 assert(!mt7932_rx_frame(packet,38,&frame)&&!frame.group4);
 ethernet[12]=8;
 assert(!mt7932_data_header(tx,ethernet,1514,0x712345678ULL,0x123,2,0,0,true));
 assert(get_unaligned_le32(tx)==0x200a060a && get_unaligned_le32(tx+4)==0x80008002);
 assert(get_unaligned_le32(tx+12)==0xf002 && get_unaligned_le16(tx+32)==0x8123);
 assert(get_unaligned_le32(tx+40)==0x12345678 && get_unaligned_le16(tx+44)==0xf5ea);
 u8 plain[64];assert(!mt7932_data_header(plain,ethernet,1514,0x712345678ULL,0x123,2,0,0,false));
 assert(get_unaligned_le32(plain+12)==0xf000);
 for(unsigned i=0;i<64;i++)assert(plain[i]==(i==12?(tx[i]&~2):tx[i]));
 assert(mt7932_data_header(tx,ethernet,1514,0x800000000ULL,0x123,2,0,0,true)<0);
 assert(mt7932_data_header(tx,ethernet,4096,0,1,2,0,0,true)<0);
 assert(mt7932_data_header(tx,ethernet,13,0,1,2,0,0,true)<0);
 assert(mt7932_data_header(tx,ethernet,1514,0,0x8000,2,0,0,true)<0);
 struct mt7932_duplicate cache[17]={0};
 frame=(struct mt7932_rx_frame){.group4=true,.unicast=true,.frame_control=0x88,.sequence_control=160};
 assert(!mt7932_rx_duplicate(cache,&frame));frame.frame_control|=0x800;
 assert(mt7932_rx_duplicate(cache,&frame));frame.sequence_control=176;
 assert(!mt7932_rx_duplicate(cache,&frame));
 frame.sequence_control=320;frame.amsdu=3;assert(!mt7932_rx_duplicate(cache,&frame));
 frame.amsdu=2;assert(!mt7932_rx_duplicate(cache,&frame));
 frame.amsdu=1;assert(!mt7932_rx_duplicate(cache,&frame));
 frame.amsdu=0;assert(mt7932_rx_duplicate(cache,&frame));
 frame.qos_control=1;assert(!mt7932_rx_duplicate(cache,&frame));
 frame.group4=false;assert(!mt7932_rx_duplicate(cache,&frame));assert(!mt7932_rx_duplicate(cache,&frame));
 u16 tokens[8];
 for(unsigned version=0;version<4;version++) {
  memset(packet,0,sizeof(packet));unsigned length=version?16:12;
  put_unaligned_le16(length,packet);put_unaligned_le16(0x3002,packet+2);
  put_unaligned_le32(version<<16,packet+4);
  if(!version){put_unaligned_le16(7,packet+8);put_unaligned_le16(8,packet+10);}
  else {put_unaligned_le32(version==3?7<<16:7,packet+8);put_unaligned_le32(version==3?8<<16:8,packet+12);}
  assert(mt7932_tx_free(packet,length,tokens,8)==2 && tokens[0]==7 && tokens[1]==8);
  assert(mt7932_tx_free(packet,length,tokens,1)<0);
  for(unsigned cut=0;cut<length;cut++)assert(mt7932_tx_free(packet,cut,tokens,8)<0);
 }
 put_unaligned_le16(16,packet);put_unaligned_le16(0x3001,packet+2);
 put_unaligned_le32(0x30000,packet+4);put_unaligned_le32(0x80000000,packet+8);put_unaligned_le32(0x01230000,packet+12);
 assert(mt7932_tx_free(packet,16,tokens,8)==1 && tokens[0]==0x123);
 put_unaligned_le32(0x80000000,packet+12);assert(mt7932_tx_free(packet,16,tokens,8)<0);
 put_unaligned_le32(4<<16,packet+4);assert(mt7932_tx_free(packet,16,tokens,8)<0);
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-packet-') as tmp:
    c, binary = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + source + tests)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    '-g', str(c), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('MT7932 packet C tests: all group combinations/truncations, integrity, IE bounds and TXP width passed')

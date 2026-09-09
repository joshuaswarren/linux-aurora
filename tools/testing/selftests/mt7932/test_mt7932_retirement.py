#!/usr/bin/env python3
"""Compile the real connection-retirement helper; inject ownership failures."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage

root = Path(__file__).resolve().parents[4]
source = local_linkage((root / 'drivers/net/wireless/mediatek/mt7932/link.c').read_text())
source = source[source.index('static void mt_peer_release'):source.index('/* Hints affect preference')]
prefix = r'''
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <errno.h>
typedef uint8_t u8;typedef uint16_t u16;typedef uint32_t u32;
#define ARRAY_SIZE(a) (sizeof(a)/sizeof((a)[0]))
#define spin_lock_irqsave(l,f) do {(void)(l);(f)=0;} while(0)
#define spin_unlock_irqrestore(l,f) do {(void)(l);(void)(f);} while(0)
#define mutex_lock(l) ((void)(l))
#define mutex_unlock(l) ((void)(l))
#define memzero_explicit memset_zero
static void memset_zero(void*p,size_t n){memset(p,0,n);}
static void __clear_bit(unsigned n,unsigned long*p){*p &= ~(1UL<<n);}
struct peer_ids {u8 station[2],wtbl[4];};
struct pci {int irq,dev;};
struct ring {unsigned queued;};
struct mt7932 {
 struct peer_ids peer_ids;unsigned long peer_alloc;
 u8 duplicates[17],connect_pmk[32],connect_bssid[6],peer_wtbl;
 bool peer_valid,disconnect_seen,disconnect_event_received,link_failed;
 bool data_live[4],data_token[4],data_retired[4];
 int netdev,data_lock,cal_work,command_mutex,response_lock,disconnect_done;
 struct ring tx[18];struct pci*pdev;
 struct {bool active;int error;} cal_state;unsigned cal_request_count,waiting,event_cipher;
};
static unsigned long jiffies;
static int fault,sent,ea42,irq_sync;
static struct mt7932 *device;
static void netif_carrier_off(int d){(void)d;}
static void netif_tx_disable(int d){(void)d;}
static void flush_work(int*w){(void)w;}
static void synchronize_irq(int irq){(void)irq;irq_sync++;}
static void reinit_completion(int*c){*c=0;}
static unsigned long msecs_to_jiffies(unsigned n){return n;}
static int time_after_eq(unsigned long a,unsigned long b){return a>=b;}
static int wait_for_completion_timeout(int*c,unsigned long t){(void)c;(void)t;return fault!=1&&fault!=13;}
static void put_unaligned_le16(u16 v,void*p){u8*d=p;d[0]=v;d[1]=v>>8;}
static void put_unaligned_le64(uint64_t v,void*p){u8*d=p;for(int i=0;i<8;i++)d[i]=v>>(i*8);}
static void mt7932_peer_queue(u8*p,u8 station,bool bind){memset(p,0,32);p[0]=15;p[2]=5;p[15]=station;assert(!bind);}
static int mt_request_ext(struct mt7932*m,u8 cid,u8 ext,bool runtime,bool set,bool wait,const void*p,size_t n){
 (void)m;const u8*b=p;assert(cid==0xea&&ext==0x42&&runtime&&set&&!wait&&n==24);
 assert(b[0]==8&&b[8]==1&&b[10]==8&&b[20]==30);ea42++;
 return fault==2?-EIO:0;
}
static int mt_request(struct mt7932*m,u8 cid,bool runtime,bool set,bool wait,const void*p,size_t n){
 (void)m;const u8*b=p;assert(cid==0x6b&&runtime&&set&&!wait&&n==32&&b[0]==15);
 sent++;assert(b[2]==(sent==1?5:1));return fault==2+sent?-EIO:0;
}
static void mt_data_clean_locked(struct mt7932*m){
 if(m->data_retired[0])m->tx[0].queued=0;
 if(!m->data_token[0]&&m->data_retired[0])m->data_live[0]=false;
}
static void mt_tx_clean(struct ring*q){if(fault!=7)q->queued=0;}
static void usleep_range(unsigned a,unsigned b){
 (void)a;(void)b;jiffies+=1000;
 if(fault!=5 && (fault!=9||jiffies>=2000))device->data_token[0]=false;
 if(fault!=6 && (fault!=10||jiffies>=2000))device->data_retired[0]=true;
 /* No reservation can be freed before both ownership observations. */
 assert(device->peer_alloc==0x8007f);
}
static void dev_info(void*p,const char*f,...){(void)p;(void)f;}
'''
main = r'''
int main(void){
 for(int mode=0;mode<14;mode++){
  struct pci pci={0};struct mt7932 m={.pdev=&pci,.peer_alloc=0x8007f,
   .peer_ids={{1,2},{3,4,5,6}},.peer_valid=true};
  device=&m;fault=mode;sent=ea42=irq_sync=0;jiffies=0;
  memset(m.connect_pmk,0xa5,32);memset(m.duplicates,0xa5,17);
  m.data_live[0]=m.data_token[0]=true;m.tx[0].queued=m.tx[17].queued=1;
  if(mode==8)m.disconnect_seen=true;
  if(mode>=12)m.disconnect_event_received=true;
  if(mode==11)m.cal_state.error=-EPROTO;
  int ret=mt_retire_connection(&m);
  bool success=mode==0||mode==8||mode==9||mode==10||mode==12;
  assert((ret==0)==success);assert(irq_sync==1);
  assert(ea42==(mode==8||mode>=12?0:1));
  if(success){
   assert(m.peer_alloc==0x80001&&!m.peer_valid&&!m.link_failed);
   for(unsigned i=0;i<32;i++)assert(!m.connect_pmk[i]);
   for(unsigned i=0;i<17;i++)assert(!m.duplicates[i]);
   assert(sent==2&&!m.data_live[0]&&!m.data_token[0]&&!m.tx[0].queued);
  }else assert(m.peer_alloc==0x8007f&&m.link_failed);
 }
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-retirement-') as tmp:
    c, binary = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + source + main)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    '-g', str(c), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Connection retirement:14 real-helper paths; remote/local/deferred, both retirement orders, command/EE44/token/descriptor/calibration failures')

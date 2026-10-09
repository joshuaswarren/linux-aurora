#!/usr/bin/env python3
"""Actual EID11 handler and TX credit helpers with ordinary stopped-qdisc model."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage

root = Path(__file__).resolve().parents[4]
source = local_linkage((root / 'drivers/net/wireless/mediatek/mt7932/data.c').read_text())
helpers = source[source.index('static bool mt_bss_tx_allowed('):source.index('static void mt_data_complete(')]
prefix = r'''
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <assert.h>
typedef uint8_t u8;
#define READ_ONCE(x) (x)
#define le32_to_cpu(x) (x)
#define DONE 0x80000000U
#define dma_rmb() ((void)0)
#define dev_dbg_ratelimited(...) ((void)0)
#define spin_lock_irqsave(l,f) do{(void)(l);(f)=0;}while(0)
#define spin_unlock_irqrestore(l,f) do{(void)(l);(void)(f);}while(0)
struct desc {uint32_t control;};
struct mt7932_ring {unsigned queued,head,tail,count;struct desc desc[4];};
struct mt7932_event {unsigned length;u8 packet[40];};
struct mt7932 {
 bool bss_absent,bss_active,connected,reg_pending,data_ready;
 u8 bss_quota;struct mt7932_ring tx[1];
 bool data_live[4],data_token[4],data_retired[4];
 int data_lock;void *netdev;
};
static bool stopped;
static unsigned wakes;
static bool mt_rf_allowed(struct mt7932*m){return !m->reg_pending;}
static void netif_stop_queue(void*n){(void)n;stopped=true;}
static void netif_wake_queue(void*n){(void)n;stopped=false;wakes++;}
'''
main = r'''
int main(void){
 struct mt7932 m={.bss_active=true,.connected=true,.data_ready=true,.tx={{.count=4}}};
 struct mt7932_event e={.length=40};e.packet[37]=1;
 mt_bss_presence(&m,&e);assert(stopped&&!mt_bss_tx_allowed(&m));
 m.tx[0].queued=1;m.tx[0].desc[0].control=DONE;m.data_live[0]=m.data_token[0]=true;
 mt_data_clean(&m);assert(stopped&&m.data_live[0]&&m.data_token[0]&&!m.bss_quota);
 e.packet[38]=2;mt_bss_presence(&m,&e);assert(m.bss_quota==2&&stopped); // head token still owned
 m.data_token[0]=false;m.data_live[0]=false;mt_data_clean(&m);assert(!stopped&&m.bss_quota==2);
 mt_bss_tx_account(&m);assert(m.bss_quota==1);
 e.packet[38]=2;mt_bss_presence(&m,&e);assert(m.bss_quota==2); // replacement, not addition
 mt_bss_tx_account(&m);mt_bss_tx_account(&m);assert(!mt_bss_tx_allowed(&m));
 e.packet[38]=0;mt_bss_presence(&m,&e);assert(stopped);
 unsigned old_wakes=wakes;mt_data_clean(&m);assert(wakes==old_wakes); // DMA retirement isn't credit
 e.packet[37]=0;mt_bss_presence(&m,&e);assert(!stopped&&mt_bss_tx_allowed(&m));
 mt_bss_tx_account(&m);assert(!m.bss_quota); // present has unlimited ordinary admission
 e.length=39;e.packet[37]=1;mt_bss_presence(&m,&e);assert(!m.bss_absent);
 e.length=40;e.packet[36]=1;mt_bss_presence(&m,&e);assert(!m.bss_absent);
 e.packet[36]=0;m.bss_active=false;mt_bss_presence(&m,&e);assert(!m.bss_absent);
 m.bss_active=true;m.connected=false;old_wakes=wakes;e.packet[37]=0;
 mt_bss_presence(&m,&e);assert(wakes==old_wakes); // presence can't restore a disconnected link
 m.connected=true;m.reg_pending=true;mt_bss_presence(&m,&e);assert(wakes==old_wakes);
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-bss-presence-') as tmp:
    c, exe = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + helpers + main)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    str(c), '-o', str(exe)], check=True)
    subprocess.run([str(exe)], check=True)
print('EID11: real replacement quota, bounded admission, presence/credit wakeup, DMA ownership, invalid/inactive BSS and teardown guards PASS')

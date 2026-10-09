#!/usr/bin/env python3
"""Execute actual netdev open/stop without requiring a radio-ready country."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage
root = Path(__file__).resolve().parents[4]
source = local_linkage((root / 'drivers/net/wireless/mediatek/mt7932/frontend.c').read_text())
code = source[source.index('static int mt_set_bss('):source.index('static const struct net_device_ops')]
prefix = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <errno.h>
typedef uint8_t u8;
#define READ_ONCE(x) (x)
#define WRITE_ONCE(x,v) ((x)=(v))
#define WLAN_REASON_DEAUTH_LEAVING 3
struct mt7932 {int command_mutex,connect_work,disconnect_work,scan_finish_work,wdev;void*wiphy;bool rf_ready,interface_up,bss_active;u8 mac[6];};
struct net_device {struct mt7932*m;};
static void*netdev_priv(struct net_device*n){return &n->m;}
static void mutex_lock(int*p){(void)p;}static void mutex_unlock(int*p){(void)p;}
static void netif_carrier_off(struct net_device*n){(void)n;}static void netif_stop_queue(struct net_device*n){(void)n;}
static void mt_disconnect(void*w,struct net_device*n,int r){(void)w;(void)n;assert(r==3);}
static void mt_abort_scan(void*w,int*d){(void)w;(void)d;}
static void flush_work(int*p){(void)p;}
static unsigned sends;static int fail;
static int mt_request(struct mt7932*m,u8 cid,bool runtime,bool set,bool wait,const void*p,unsigned n){
 const u8*b=p;assert(cid==0x11&&runtime&&set&&!wait&&n==12);sends++;
 if(b[1])assert(!memcmp(b+4,m->mac,6));else assert(!memcmp(b+4,(u8[6]){0},6));
 return fail?-EIO:0;
}
'''
main = r'''
int main(void){
 struct mt7932 m={.mac={2,1,2,3,4,5}};struct net_device n={.m=&m};
 assert(!mt_net_open(&n)&&m.interface_up&&!m.bss_active&&!sends);
 assert(!mt_net_stop(&n)&&!m.interface_up&&!sends);
 m.rf_ready=true;assert(!mt_net_open(&n)&&m.interface_up&&m.bss_active&&sends==1);
 assert(!mt_net_stop(&n)&&!m.interface_up&&!m.bss_active&&sends==2);
 fail=1;assert(mt_net_open(&n)==-EIO&&!m.interface_up&&!m.bss_active&&sends==3);
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-interface-') as tmp:
    c, exe = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + code + main)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    str(c), '-o', str(exe)], check=True)
    subprocess.run([str(exe)], check=True)
print('Real netdev lifecycle: world-policy open/stop without firmware RF, ready BSS activation/deactivation, command failure PASS')

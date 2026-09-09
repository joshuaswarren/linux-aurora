#!/usr/bin/env python3
"""Execute the real connect worker, cancellation adapter and internal scan."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage

root = Path(__file__).resolve().parents[4]
driver = root / 'drivers/net/wireless/mediatek/mt7932'
link = local_linkage((driver / 'link.c').read_text())
frontend = local_linkage((driver / 'frontend.c').read_text())
worker = link[link.index('static void mt_connect_work'):link.index('/* Narrow ordinary OPEN')]
cancel = link[link.index('static int mt_disconnect('):link.index('static int mt_change_interface')]
discovery = frontend[frontend.index('static int mt_connect_discover(struct mt7932 *m)\n{'):frontend.index('static int mt_scan_admit(')]

prefix = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <stdio.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
#define ETH_ALEN 6
#define IEEE80211_CHAN_DISABLED 1
#define IEEE80211_CHAN_NO_IR 2
#define IEEE80211_CHAN_NO_20MHZ 4
#define WLAN_STATUS_UNSPECIFIED_FAILURE 1
#define WLAN_STATUS_SUCCESS 0
#define GFP_KERNEL 0
#define GFP_ATOMIC 1
#define READ_ONCE(x) (x)
#define container_of(p,t,m) ((t *)((char *)(p)-offsetof(t,m)))
#define dev_info(...) ((void)0)
#define dev_err(...) ((void)0)
struct net_device {int unused;}; struct wiphy {int unused;};
struct work_struct {int unused;}; struct completion {int kind;};
struct ieee80211_channel {unsigned flags;u8 hw_value;};
struct ieee80211_supported_band {unsigned n_channels;struct ieee80211_channel *channels;};
struct cfg80211_bss {struct ieee80211_channel *channel;u8 bssid[6];};
struct cfg80211_connect_resp_params {int status;struct {struct cfg80211_bss*bss;const u8*bssid;} links[1];};
struct cfg80211_deauth_request {const u8*bssid;size_t ie_len;bool local_state_change;u16 reason_code;};
struct peer_ids {u8 station[2],wtbl[4];};
struct mt7932 {
 struct work_struct connect_work,disconnect_work;
 struct net_device *netdev;struct wiphy*wiphy;int pdev;
 int command_mutex,response_lock,connect_error;
 struct completion assoc_start,assoc_done,disconnect_done,discovery_done;
 struct cfg80211_bss *connect_bss;
 struct ieee80211_channel *connect_channel_req;
 struct ieee80211_supported_band band2,band5;
 bool rf_ready,connecting,connected,connect_cancelled,connect_open,disconnecting;
 bool peer_valid,link_failed,data_ready,disconnect_seen,disconnect_event_received,disconnect_local;
 bool discovering,discovery_finished;
 u16 disconnect_reason;
 u8 connect_pmk[32],connect_ssid[32],connect_bssid[6],mac[6],connect_channel,scan_seq;
 size_t connect_ssid_length;
 struct peer_ids peer_ids;
};
static struct mt7932 *device;
static bool mt_rf_allowed(struct mt7932*m){return m->rf_ready;}
static int mode,terminal,authorized,carrier,retired,released,reserved,scan_commands,cancels;
static int final_inject,band_seen;
static bool mutex_held;
static struct ieee80211_channel channels2[]={{0,4}},channels5[]={{0,36}};
static struct cfg80211_bss bss={.channel=channels2,.bssid={2,1,2,3,4,5}};
static int mt_disconnect(struct wiphy*,struct net_device*,u16);
static void lock_response(void){
 if(final_inject){final_inject=0;device->connect_error=-ECANCELED;device->connect_cancelled=true;}
}
#define spin_lock_irqsave(l,f) do{(void)(l);(f)=0;lock_response();}while(0)
#define spin_unlock_irqrestore(l,f) do{(void)(l);(void)(f);}while(0)
static void mutex_lock(int*p){(void)p;assert(!mutex_held);mutex_held=true;}
static void mutex_unlock(int*p){(void)p;assert(mutex_held);mutex_held=false;}
static void *kzalloc(size_t n,int g){(void)g;return calloc(1,n);}
static void kfree_sensitive(void*p){free(p);}
static void memzero_explicit(void*p,size_t n){memset(p,0,n);}
static void reinit_completion(struct completion*c){(void)c;}
static void complete(struct completion*c){(void)c;}
static unsigned long msecs_to_jiffies(unsigned long t){return t;}
static int wait_for_completion_timeout(struct completion*c,unsigned long t){
 (void)t;assert(!mutex_held);
 if(c==&device->discovery_done){
  if(mode==7)return 0;
  if(mode==6 && !device->connect_cancelled){mt_disconnect(device->wiphy,device->netdev,3);return 1;}
  device->discovery_finished=true;return 1;
 }
 if(c==&device->assoc_start && (mode==2||mode==8))mt_disconnect(device->wiphy,device->netdev,3);
 if(c==&device->assoc_done && mode==3)final_inject=1;
 return 1;
}
static void schedule_work(struct work_struct*w){(void)w;}
static void flush_work(struct work_struct*w){(void)w;}
static struct mt7932 *mt_from_wiphy(struct wiphy*w){(void)w;return device;}
static bool ether_addr_equal(const u8*a,const u8*b){return !memcmp(a,b,6);}
static void netif_carrier_off(struct net_device*n){(void)n;carrier=0;}
static void netif_stop_queue(struct net_device*n){(void)n;}
static void netif_carrier_on(struct net_device*n){(void)n;assert(!device->connect_cancelled);carrier++;}
static void netif_wake_queue(struct net_device*n){(void)n;}
static int mt_peer_reserve(struct mt7932*m){(void)m;reserved++;return 0;}
static void mt_peer_release(struct mt7932*m){(void)m;released++;}
static int mt_retire_connection(struct mt7932*m){assert(!mutex_held&&m->disconnecting);retired++;if(mode==8)m->link_failed=true;return mode==8?-EIO:0;}
static void mt_transport_snapshot(struct mt7932*m){(void)m;}
static int mt7932_connect_body(u8*b,size_t n,const u8*s,size_t sn,const u8*k,const u8*a,u8 ch,unsigned l,u8 f,const struct peer_ids*p,bool o){
 (void)b;(void)n;(void)s;(void)sn;(void)k;(void)a;(void)ch;(void)l;(void)f;(void)p;(void)o;return 856;
}
static int mt_association_calibration(struct mt7932*m){(void)m;return 0;}
static void mt7932_peer_queue(u8*p,int s,bool b){(void)p;(void)s;(void)b;}
static int mt_request(struct mt7932*m,u8 cid,bool a,bool b,bool c,const void*p,size_t n){
 (void)a;(void)b;(void)c;(void)p;(void)n;assert(mutex_held);
 if(cid==3)scan_commands++;
 if(cid==0x1b)cancels++;
 if(cid==0xb8 && mode==1)mt_disconnect(m->wiphy,m->netdev,3);
 return 0;
}
static int mt_request_ext(struct mt7932*m,u8 cid,u8 ex,bool a,bool b,bool c,const void*p,size_t n){
 (void)m;(void)cid;(void)ex;(void)a;(void)b;(void)c;(void)p;(void)n;assert(!device->connect_cancelled);return 0;
}
static int mt7932_scan_body(u8*b,size_t n,u8 seq,const u8*c,unsigned count){
 (void)b;(void)n;(void)seq;assert(count==1);band_seen=c[0];return 0;
}
static int mt7932_scan_options(u8*b,const u8*s,size_t sn,const u8*ie,size_t in){
 (void)b;(void)s;(void)sn;assert(!ie&&!in);return 0;
}
static struct cfg80211_bss *mt_connect_find_bss(struct mt7932*m){
 (void)m;if(mode==4||mode==6||mode==7)return NULL;
 if(mode==5 && band_seen!=36)return NULL;
 return &bss;
}
static void cfg80211_put_bss(struct wiphy*w,struct cfg80211_bss*b){(void)w;assert(b==&bss);}
static void cfg80211_connect_done(struct net_device*n,struct cfg80211_connect_resp_params*r,int g){
 (void)n;assert(g==GFP_ATOMIC);assert(!device->connect_cancelled);terminal++;if(!r->status)assert(r->links[0].bss==&bss);
}
static void cfg80211_disconnected(struct net_device*n,u16 reason,const u8*ie,size_t len,bool local,int g){
 (void)n;(void)reason;(void)ie;(void)len;assert(local&&g==GFP_ATOMIC);terminal++;
}
static void cfg80211_port_authorized(struct net_device*n,const u8*b,const u8*p,size_t len,int g){
 (void)n;(void)b;(void)p;(void)len;assert(g==GFP_ATOMIC&&!device->connect_cancelled);authorized++;
}
static int mt_connect_discover(struct mt7932*m);
'''
main = r'''
int main(void){
 for(mode=0;mode<=11;mode++){
  struct net_device n={0};struct wiphy w={0};struct mt7932 m={.netdev=&n,.wiphy=&w,.rf_ready=true,.data_ready=true,.connecting=true};
  device=&m;terminal=authorized=carrier=retired=released=reserved=scan_commands=cancels=final_inject=band_seen=0;mutex_held=false;
  m.band2=(struct ieee80211_supported_band){1,channels2};m.band5=(struct ieee80211_supported_band){1,channels5};
  channels5[0].flags=mode==11?IEEE80211_CHAN_DISABLED:0;
  bss.channel=(mode==5||mode>=10)?channels5:channels2;
  m.connect_bss=((mode>=4&&mode<=7)||mode>=10)?NULL:&bss;
  if(mode>=10)m.connect_channel_req=channels5;
  memset(m.connect_pmk,0xa5,32);memcpy(m.connect_bssid,bss.bssid,6);
  if(mode==9){m.connect_error=-ECANCELED;m.connect_cancelled=true;}
  mt_connect_work(&m.connect_work);
  bool ok=mode==0||mode==5||mode==10;
  assert(!mutex_held&&!m.connecting&&!m.discovering&&terminal==1);
  assert((m.connected==ok)&&(authorized==ok)&&(carrier==ok));
  assert(!m.connect_bss);
  for(unsigned i=0;i<32;i++)assert(!m.connect_pmk[i]);
  if(mode==1||mode==2||mode==3)assert(retired==1);
  if(mode==4)assert(scan_commands==2&&!reserved);
  if(mode==5)assert(scan_commands==2&&reserved==1);
  if(mode==6)assert(cancels==1&&!m.link_failed&&!reserved);
  if(mode==7)assert(cancels==1&&m.link_failed&&!reserved);
  if(mode==8)assert(retired==1&&m.link_failed);
  if(mode==10)assert(scan_commands==1&&m.connect_channel==36);
  if(mode==11)assert(!scan_commands&&!reserved);
  m.connecting=true;m.connected=false;m.connect_cancelled=false;m.connect_error=0;
  struct cfg80211_deauth_request req={.bssid=bss.bssid,.reason_code=3};
  req.ie_len=1;assert(mt_deauth(&w,&n,&req)==-EOPNOTSUPP&&!m.connect_cancelled);
  req.ie_len=0;req.local_state_change=true;assert(mt_deauth(&w,&n,&req)==-EOPNOTSUPP);
  req.local_state_change=false;u8 other[6]={2,2,3,4,5,6};req.bssid=other;assert(mt_deauth(&w,&n,&req)==-ENOTCONN);
  req.bssid=bss.bssid;assert(!mt_deauth(&w,&n,&req)&&m.connect_cancelled&&m.connect_error==-ECANCELED);
 }
 puts("Real connect/discovery/deauth: success, missing BSS, dual-band discovery, early/handshake/final cancellation, timeout ownership, request validation PASS");
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-reconnect-') as tmp:
    c, binary = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + worker + cancel + discovery + main)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    '-g', str(c), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)

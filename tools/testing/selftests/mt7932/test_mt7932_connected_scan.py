#!/usr/bin/env python3
"""Real connected batch sender: full caller scope, home context, no peer changes."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage

root = Path(__file__).resolve().parents[4]
driver = root / 'drivers/net/wireless/mediatek/mt7932'
source = local_linkage((driver / 'frontend.c').read_text())
sender = source[source.index('static int mt_scan_submit_next(struct mt7932 *m)\n{'):
                source.index('/* Internal, disconnected discovery')]
entry = source[source.index('static int mt_scan(struct wiphy *wiphy,'):
               source.index('/* command_mutex held; host interface existence')]
headers = (driver / 'channels.h').read_text() + '\n' + '\n'.join(
    line for line in (driver / 'scan.h').read_text().splitlines()
    if not line.startswith('#include'))
prefix = r'''
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <string.h>
#include <errno.h>
#include <assert.h>
typedef uint8_t u8;typedef uint16_t u16;typedef uint32_t u32;
#define ARRAY_SIZE(x) (sizeof(x)/sizeof((x)[0]))
#define dev_info(...) ((void)0)
#define spin_lock_irqsave(l,f) do{(void)(l);(f)=0;}while(0)
#define spin_unlock_irqrestore(l,f) do{(void)(l);(void)(f);}while(0)
static void put_unaligned_le16(u16 v,void*p){u8*b=p;b[0]=v;b[1]=v>>8;}
static void put_unaligned_le32(u32 v,void*p){u8*b=p;for(unsigned i=0;i<4;i++)b[i]=v>>(i*8);}
#define IEEE80211_CHAN_NO_IR 1
#define IEEE80211_CHAN_DISABLED 2
#define IEEE80211_CHAN_NO_20MHZ 4
#define NL80211_SCAN_FLAG_FLUSH 1
#define NL80211_SCAN_FLAG_COLOCATED_6GHZ 2
#define NL80211_BAND_2GHZ 0
#define NL80211_BAND_5GHZ 1
struct ieee80211_channel {u8 hw_value;unsigned flags,band;};
struct wiphy {int unused;};
struct cfg80211_ssid {u8 ssid[32];unsigned ssid_len;};
struct cfg80211_scan_request {
 unsigned n_channels,n_ssids,ie_len;struct ieee80211_channel *channels[17];
 struct cfg80211_ssid *ssids;u8 *ie;
 void *wdev;unsigned flags,duration;bool duration_mandatory;
};
struct mt7932 {
 struct cfg80211_scan_request *scan_request;
 bool scan_aborted,connected,disconnecting,reg_pending,scan_finished;
 u8 scan_band,scan_batch,scan_seq,scan_home_channel,connect_channel;
 int response_lock,scan_done,scan_timeout_work,command_mutex,wdev;
 unsigned peer_id;bool keys,carrier;
};
static unsigned sent;static u8 command[0x4d4];
static bool mt_rf_allowed(struct mt7932*m){return !m->reg_pending;}
static struct mt7932 *mt_from_wiphy(struct wiphy *w){return (void *)w;}
static int mt_scan_admit(struct mt7932 *m){(void)m;return 0;}
static void mutex_lock(int *m){(void)m;}
static void mutex_unlock(int *m){(void)m;}
static void cancel_delayed_work(int *w){(void)w;}
static void reinit_completion(int*c){*c=0;}
static unsigned msecs_to_jiffies(unsigned n){return n;}
static void schedule_delayed_work(int*w,unsigned n){(void)w;assert(n==60000);}
static int mt_request(struct mt7932*m,u8 cid,bool runtime,bool set,bool reply,void*b,size_t n){
 assert(cid==3&&runtime&&set&&!reply&&n==sizeof(command));
 assert(m->peer_id==5&&m->keys&&m->carrier);memcpy(command,b,n);sent++;return 0;
}
'''
main = r'''
int main(void){
 struct ieee80211_channel channels[17]={0};struct cfg80211_scan_request r={.n_channels=17};
 for(unsigned i=0;i<17;i++){channels[i].hw_value=i<13?i+1:36+4*(i-13);r.channels[i]=&channels[i];}
 struct mt7932 m={.scan_request=&r,.connected=true,.scan_home_channel=36,.peer_id=5,.keys=true,.carrier=true};
 assert(!mt_scan_submit_next(&m));assert(sent==1&&command[0x4cc]==36&&command[0x4cd]==2&&command[0x9f]==13);
 assert(!command[2]&&!command[3]&&!command[4]&&!command[5]&&command[0x9a]==40);
 for(unsigned i=0;i<13;i++)assert(command[0xa0+2*i]==1&&command[0xa1+2*i]==i+1);
 assert(!mt_scan_submit_next(&m));assert(sent==2&&command[0x9f]==4);
 for(unsigned i=0;i<4;i++)assert(command[0xa0+2*i]==2&&command[0xa1+2*i]==36+4*i);
 assert(mt_scan_submit_next(&m)==1&&sent==2&&m.peer_id==5&&m.keys&&m.carrier);
 /* A small explicit caller list isn't substituted with the home channel. */
 r.n_channels=1;m.scan_batch=0;m.scan_home_channel=48;
 assert(!mt_scan_submit_next(&m));assert(command[0x9f]==1&&command[0xa1]==1&&command[0x4cc]==48);
 m.scan_batch=0;m.disconnecting=true;assert(mt_scan_submit_next(&m)==-ECANCELED);
 m.disconnecting=false;m.connected=false;m.scan_batch=0;assert(mt_scan_submit_next(&m)==-ECANCELED);
 m.connected=true;m.reg_pending=true;m.scan_batch=0;assert(mt_scan_submit_next(&m)==-ECANCELED);
 assert(sent==3&&m.peer_id==5&&m.keys&&m.carrier);
 /* A directed request splits NO_IR from active channels in each band.
  * Each requested channel occurs once, never as an active NO_IR probe.
  */
 struct cfg80211_ssid ssid={.ssid="TEST",.ssid_len=4};
 r.n_channels=17;r.n_ssids=1;r.ssids=&ssid;
 channels[12].flags=IEEE80211_CHAN_NO_IR;
 channels[16].flags=IEEE80211_CHAN_NO_IR;
 m.reg_pending=false;m.scan_batch=0;
 unsigned seen[49]={0};u8 prev=m.scan_seq;
 for(unsigned batch=0;batch<4;batch++){
  assert(!mt_scan_submit_next(&m)&&m.scan_seq!=prev);prev=m.scan_seq;
  bool passive=batch&1;
  assert(command[2]==!passive&&command[0x9a]==(passive?40:20));
  assert(command[0x4cc]==48&&command[0x4cd]==2);
  if(passive){
   assert(!command[3]&&!command[4]&&!command[5]);
   for(unsigned i=8;i<152;i++)assert(!command[i]);
   for(unsigned i=0x458;i<0x464;i++)assert(!command[i]);
  }else assert(command[3]==8&&command[4]==1&&!memcmp(command+12,"TEST",4));
  for(unsigned i=0;i<command[0x9f];i++){
   unsigned ch=command[0xa1+2*i];assert(ch<49);seen[ch]++;
   if(ch==13||ch==48)assert(passive);
   else assert(!passive);
  }
 }
 assert(mt_scan_submit_next(&m)==1);
 for(unsigned i=0;i<17;i++)assert(seen[channels[i].hw_value]==1);
 /* Explicit passive covers both NO_IR and unrestricted channels. */
 r.n_ssids=0;m.scan_batch=0;
 assert(!mt_scan_submit_next(&m)&&!command[2]&&command[0x9f]==13);
 assert(!mt_scan_submit_next(&m)&&!command[2]&&command[0x9f]==4);
 assert(mt_scan_submit_next(&m)==1);
 /* A new restriction between batches is honored, not snapshotted away. */
 r.n_ssids=1;channels[12].flags=channels[16].flags=0;m.scan_batch=0;
 assert(!mt_scan_submit_next(&m)&&command[2]==1&&command[0x9f]==13);
 for(unsigned i=13;i<17;i++)channels[i].flags=IEEE80211_CHAN_NO_IR;
 assert(!mt_scan_submit_next(&m)&&!command[2]&&command[0x9f]==4);
 assert(mt_scan_submit_next(&m)==1);
 /* Disabled/unusable channels never reach firmware. */
 unsigned before=sent;r.n_channels=1;m.scan_batch=0;
 channels[0].flags=IEEE80211_CHAN_DISABLED;
 assert(mt_scan_submit_next(&m)==-EINVAL&&sent==before);
 channels[0].flags=IEEE80211_CHAN_NO_20MHZ;m.scan_batch=0;
 assert(mt_scan_submit_next(&m)==-EINVAL&&sent==before);
 /* Execute the real cfg80211 entry, not only the batch builder: NO_IR
  * is admitted for listening, while DISABLED/NO_20MHZ stay rejected.
  */
 r.wdev=&m.wdev;r.n_ssids=1;channels[0].flags=IEEE80211_CHAN_NO_IR;
 assert(!mt_scan((void *)&m,&r)&&sent==before+1&&!command[2]);
 m.scan_request=NULL;before=sent;
 channels[0].flags=IEEE80211_CHAN_DISABLED;
 assert(mt_scan((void *)&m,&r)==-EINVAL&&sent==before);
 channels[0].flags=IEEE80211_CHAN_NO_20MHZ;
 assert(mt_scan((void *)&m,&r)==-EINVAL&&sent==before);
 channels[0].flags=0;r.n_ssids=0;
 assert(!mt_scan((void *)&m,&r)&&sent==before+1&&!command[2]);
 m.scan_request=NULL;before=sent;r.n_ssids=1;
 assert(!mt_scan((void *)&m,&r)&&sent==before+1&&command[2]==1);
 m.scan_request=NULL;before=sent;
 r.duration_mandatory=true;
 assert(mt_scan((void *)&m,&r)==-EOPNOTSUPP&&sent==before);
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-connected-scan-') as tmp:
    c, exe = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + headers + '\n' + sender + entry + main)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    str(c), '-o', str(exe)], check=True)
    subprocess.run([str(exe)], check=True)
print('Connected scan: actual sender preserves full caller scope/home channel/peer, excludes teardown and policy races PASS')

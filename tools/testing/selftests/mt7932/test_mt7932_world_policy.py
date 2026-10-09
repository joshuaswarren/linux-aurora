#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Execute world policy selection/publication, without firmware or hardware.

Synthetic packages exercise wire validation only, never supply RF power data.
Optional arguments validate the same code with privately generated XZ/PL files.
"""
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
from mt7932_test_lib import local_linkage

root = Path(__file__).resolve().parents[4]
driver = root / 'drivers/net/wireless/mediatek/mt7932'
source = local_linkage((driver / 'startup.c').read_text())
assert 'm->wiphy->flags |= WIPHY_FLAG_CHANNEL_CHANGE_ON_BEACON;' in source
publish = source[source.index('static int mt_publish_policy('):
                 source.index('static int mt_privacy_defaults(')]
worker = source[source.index('static void mt_startup_work('):
                source.index('static const struct cfg80211_ops')]


def package(country):
    """Format-only synthetic fixtures. Zero rate bytes are NOT RF limits."""
    modes = b''.join(struct.pack('<II', 0, len(command)) + command.ljust(320, b'\0')
                     for command in (b'SarEnableConfig 1', b'coex tasar_set 0 0'))
    bodies = []
    for i in range(13):
        if 1 <= i <= 9:
            count = (6 if country == b'XZ' else 5) if i == 2 else 5 if i == 9 else 8
            body = bytearray(44 + count * 122)
            body[:2] = b'\x03\x0a'
            body[4:7] = bytes((count, 1 if i <= 2 else 2, i in (2, 9)))
            body[8:10] = country
            if i <= 2:
                for j in range(count):
                    body[44 + j * 122] = (i - 1) * 8 + j + 1
        else:
            k = i - 9 if i else 0
            body = bytearray((532, 144, 58, 265)[k])
            body[1] = 2
            body[7] = (5, 1, 4, 2)[k]
            if i == 10:
                body[8:10] = country
        struct.pack_into('<H', body, 2, len(body))
        bodies.append(body)
    payload = modes + b''.join(bodies)
    return struct.pack('<4sH2sBB2xI', b'J7RP', 1, country, 2, 13, 16 + len(payload)) + payload


prefix = r'''
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <assert.h>
typedef uint8_t u8;typedef uint16_t u16;typedef uint32_t u32;typedef int32_t s32;
static u16 get_unaligned_le16(const void *v){const u8*p=v;return p[0]|p[1]<<8;}
static u32 get_unaligned_le32(const void *v){const u8*p=v;return (u32)p[0]|(u32)p[1]<<8|(u32)p[2]<<16|(u32)p[3]<<24;}
static void put_unaligned_le16(u16 v,void*p){u8*b=p;b[0]=v;b[1]=v>>8;}
static void put_unaligned_le32(u32 v,void*p){u8*b=p;for(unsigned i=0;i<4;i++)b[i]=v>>(8*i);}
'''
mocks = r'''
#define READ_ONCE(x) (x)
#define WRITE_ONCE(x,v) ((x)=(v))
#define container_of(p,t,m) ((t *)((char *)(p)-offsetof(t,m)))
#define dev_info(...) ((void)0)
#define dev_warn(...) ((void)0)
#define WLAN_REASON_DEAUTH_LEAVING 3
#define scnprintf snprintf
#define spin_lock_irqsave(l,f) do{(void)(l);(f)=0;}while(0)
#define spin_unlock_irqrestore(l,f) do{(void)(l);(void)(f);}while(0)
#define IEEE80211_CHAN_DISABLED 1
#define IEEE80211_CHAN_RADAR 8
#define IEEE80211_CHAN_NO_OFDM 64
#define IEEE80211_CHAN_NO_IR 2
#define IEEE80211_CHAN_NO_20MHZ 2048
#define IEEE80211_CHAN_PSD 4
#define IEEE80211_CHAN_IR_CONCURRENT 1024
struct wiphy {int unused;};
struct regulatory_request {u8 alpha2[2];unsigned initiator;};
struct ieee80211_channel {u32 flags;int max_power;u16 hw_value;};
struct work_struct {int x;};struct pci {int dev;};
struct mt7932 {
 struct work_struct startup_work,scan_finish_work,cal_work;
 int response_lock,command_mutex,scan_done,assoc_start,assoc_done,discovery_done,connect_error;
 u32 reg_generation,reg_attempted;
 struct mt7932_reg_snapshot reg_desired;
 void *netdev,*wiphy,*scan_request;int wdev;
 struct pci *pdev;
 bool reg_pending,policy_failed,link_failed,rf_ready,startup_started,interface_registered,connecting;
 struct ieee80211_channel channels[13],channels5[4];u8 retired_scan_seq;
};
struct firmware {const u8 *data;size_t size;};
static struct firmware world,pl;
static int sent,started,disconnected,released,fail_at,domains,missing,supersede;
static char last_domain[2],last_path[80];
static struct mt7932 *owner;
static struct mt7932 *mt_from_wiphy(struct wiphy*w){return (void*)w;}
static void netif_stop_queue(void*n){(void)n;}
static void complete(int*c){(void)c;}
static void schedule_work(struct work_struct*w){(void)w;}
static void mutex_lock(int*p){(void)p;}static void mutex_unlock(int*p){(void)p;}
static void flush_work(struct work_struct*p){(void)p;}
static int mt_disconnect(void*w,void*n,int r){(void)w;(void)n;assert(r==3);disconnected++;return 0;}
static void mt_abort_scan(void*w,void*d){(void)w;(void)d;}
static unsigned msecs_to_jiffies(unsigned n){return n;}
static int wait_for_completion_timeout(int*c,unsigned n){(void)c;(void)n;return 1;}
static int request_firmware_direct(const struct firmware**f,const char*path,void*d){
 (void)d;assert(strlen(path)<sizeof(last_path));strcpy(last_path,path);
 if(!strcmp(path,"mediatek/mt7932/policy/world-XZ.bin")){
  if(missing)return -ENOENT;*f=&world;
 }else if(!strcmp(path,"mediatek/mt7932/policy/PL.bin"))*f=&pl;
 else return -ENOENT;
 if(supersede){supersede=0;owner->reg_generation++;
  memcpy(owner->reg_desired.alpha2,"PL",2);memcpy(owner->reg_desired.domain,"PL",2);}
 return 0;
}
static void release_firmware(const struct firmware*f){assert(f==&world||f==&pl);released++;}
static int mt_request(struct mt7932*m,u8 cid,bool runtime,bool set,bool reply,const void*data,size_t n){
 const u8*p=data;assert(runtime&&set&&!reply&&m->reg_pending);sent++;
 if(fail_at&&sent==fail_at)return -EIO;
 if(cid==0x0f){
  domains++;memcpy(last_domain,p,2);assert(n==140&&p[8]==12&&p[9]==4);
  for(unsigned i=12;i<n;i+=8){unsigned ch=get_unaligned_le16(p+i);
   assert(ch!=1&&ch!=14);if(ch==13)assert(get_unaligned_le32(p+i+4)&IEEE80211_CHAN_NO_IR);}
 }
 if(cid==0x5d&&((p[0]==3&&p[1]==0x0a)||p[7]==1))assert(!memcmp(p+8,last_domain,2));
 return 0;
}
static int mt_startup_once(struct mt7932*m){assert(!m->startup_started);m->startup_started=true;m->rf_ready=true;m->netdev=m;started++;return 0;}
'''
main = r'''
static struct pci pci;
static struct mt7932 fresh(void){
 struct mt7932 m={.pdev=&pci,.interface_registered=true};
 for(unsigned i=0;i<13;i++)m.channels[i]=(struct ieee80211_channel){.max_power=20,.hw_value=i+1};
 for(unsigned i=0;i<4;i++)m.channels5[i]=(struct ieee80211_channel){.max_power=20,.hw_value=36+4*i};
 m.channels[0].flags=IEEE80211_CHAN_DISABLED;m.channels[12].flags=IEEE80211_CHAN_NO_IR;return m;
}
static void hint(struct mt7932*m,const char*cc){
 struct regulatory_request r={0};memcpy(r.alpha2,cc,2);owner=m;
 mt_regulatory_notify((void*)m,&r);mt_startup_work(&m->startup_work);
}
int main(int argc,char**argv){
 assert(argc==3);u8 w[16384],p[16384];FILE*f=fopen(argv[1],"rb");assert(f);
 world.size=fread(w,1,sizeof(w),f);fclose(f);world.data=w;
 f=fopen(argv[2],"rb");assert(f);pl.size=fread(p,1,sizeof(p),f);fclose(f);pl.data=p;
 struct mt7932_policy policy;
 assert(!mt7932_policy_parse(&policy,w,world.size,(const u8*)"XZ"));
 assert(policy.length[2]==776);
 u8 *second=(u8 *)policy.table[2];u8 saved=second[44];second[44]=14;
 assert(mt7932_policy_parse(&policy,w,world.size,(const u8*)"XZ")==-EINVAL);second[44]=saved;
 second[12]^=1;assert(mt7932_policy_parse(&policy,w,world.size,(const u8*)"XZ")==-EINVAL);second[12]^=1;
 assert(mt7932_policy_parse(&policy,w,world.size,(const u8*)"00")==-EINVAL);
 assert(!mt7932_policy_parse(&policy,p,pl.size,(const u8*)"PL"));
 assert(policy.length[2]==654);
 for(size_t i=0;i<world.size;i++)assert(mt7932_policy_parse(&policy,w,i,(const u8*)"XZ")==-EINVAL);
 struct mt7932 m=fresh();hint(&m,"00");
 assert(!m.reg_pending&&m.rf_ready&&started==1&&domains==1);
 assert(!memcmp(m.reg_desired.alpha2,"00",2)&&!memcmp(last_domain,"XZ",2));
 assert(!strcmp(last_path,"mediatek/mt7932/policy/world-XZ.bin"));
 int before=sent;hint(&m,"00");assert(sent==before);
 hint(&m,"PL");assert(!m.reg_pending&&started==1&&domains==2&&!memcmp(last_domain,"PL",2));
 hint(&m,"00");assert(!m.reg_pending&&started==1&&domains==3&&!memcmp(last_domain,"XZ",2));
 /* Same-country beacon updates must republish the changed channel flags. */
 m.channels5[0].flags=IEEE80211_CHAN_NO_IR;before=domains;hint(&m,"00");
 assert(!m.reg_pending&&domains==before+1);
 assert(get_unaligned_le32(m.reg_desired.domain+16+12*8)&IEEE80211_CHAN_NO_IR);
 m.channels5[0].flags=0;hint(&m,"00");
 assert(!m.reg_pending&&domains==before+2);
 assert(!(get_unaligned_le32(m.reg_desired.domain+16+12*8)&IEEE80211_CHAN_NO_IR));
 /* Missing/corrupt world policy is recoverable, never another-country fallback. */
 m=fresh();before=sent;missing=1;hint(&m,"00");
 assert(m.reg_pending&&!m.policy_failed&&!m.startup_started&&sent==before);
 hint(&m,"PL");assert(!m.reg_pending&&m.rf_ready);missing=0;
 m=fresh();before=sent;w[6]='0';hint(&m,"00");
 assert(m.reg_pending&&!m.policy_failed&&sent==before);w[6]='X';
 hint(&m,"00");assert(!m.reg_pending);
 before=sent;hint(&m,"ZZ");assert(m.reg_pending&&!m.policy_failed&&sent==before);
 assert(!strcmp(last_path,"mediatek/mt7932/policy/ZZ.bin"));
 /* An obsolete world program cannot publish or reopen admission. */
 m=fresh();before=domains;supersede=1;hint(&m,"00");
 assert(!m.reg_pending&&domains==before+1&&!memcmp(last_domain,"PL",2));
 /* Ambiguous scan ownership and unsupported numerical limits stay gated. */
 m=fresh();before=sent;m.retired_scan_seq=1;hint(&m,"00");assert(m.reg_pending&&sent==before);
 m.retired_scan_seq=0;m.channels[1].max_power=10;hint(&m,"00");assert(m.reg_pending&&sent==before);
 m.channels[1].max_power=20;hint(&m,"00");assert(!m.reg_pending);
 /* Partial publication cannot be recovered by simply changing countries. */
 m=fresh();fail_at=sent+4;hint(&m,"00");assert(m.reg_pending&&m.policy_failed);
 before=sent;fail_at=0;hint(&m,"PL");assert(m.reg_pending&&sent==before);
 assert(disconnected&&released);
}
'''
if len(sys.argv) not in (1, 3):
    raise SystemExit('usage: test_mt7932_world_policy.py [XZ_POLICY PL_POLICY]')
with tempfile.TemporaryDirectory(prefix='mt7932-world-policy-') as directory:
    tmp = Path(directory)
    paths = [tmp / 'XZ.bin', tmp / 'PL.bin']
    for path, country in zip(paths, (b'XZ', b'PL')):
        path.write_bytes(package(country))
    c, exe = tmp / 'test.c', tmp / 'test'
    c.write_text(prefix + (driver / 'regulatory.h').read_text() + mocks + publish + worker + main)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-Wno-misleading-indentation',
                    '-fsanitize=address,undefined', '-g', str(c), '-o', str(exe)], check=True)
    subprocess.run([str(exe), *map(str, paths)], check=True)
    if len(sys.argv) == 3:
        subprocess.run([str(exe), *sys.argv[1:]], check=True)
print('World policy: actual notifier/worker/publication, explicit XZ identity, world restrictions, '
      'transitions, malformed/missing files, obsolete generations and partial-failure gates PASS')

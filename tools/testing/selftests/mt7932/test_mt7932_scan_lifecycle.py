#!/usr/bin/env python3
"""Actual scan abort/event handlers: local completion is not a firmware ACK."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage
root = Path(__file__).resolve().parents[4]
source = local_linkage((root / 'drivers/net/wireless/mediatek/mt7932/frontend.c').read_text())
finish = source[source.index('static void mt_scan_finish_work('):source.index('static void mt_scan_timeout_work(')]
event = source[source.index('static void mt_scan_event('):source.index('static void mt_abort_scan(')]
quiesce = source[source.index('static void mt_abort_scan('):source.index('/* command_mutex held:')]
link = local_linkage((root / 'drivers/net/wireless/mediatek/mt7932/link.c').read_text())
disconnect = link[link.index('static void mt_disconnect_work('):link.index('static int mt_disconnect(')]
prefix = r'''
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <assert.h>
#include <errno.h>
typedef uint8_t u8;typedef uint16_t u16;
#define GFP_ATOMIC 0
#define container_of(p,t,m) ((t *)((char *)(p)-offsetof(t,m)))
#define READ_ONCE(x) (x)
#define WRITE_ONCE(x,v) ((x)=(v))
#define dev_info(...) ((void)0)
#define dev_err(...) ((void)0)
#define spin_lock_irqsave(l,f) do{(void)(l);(f)=0;}while(0)
#define spin_unlock_irqrestore(l,f) do{(void)(l);(void)(f);}while(0)
struct work_struct{int x;};struct cfg80211_scan_request{int x;};
struct wiphy{int x;};struct wireless_dev{int x;};
struct cfg80211_scan_info{bool aborted;};struct mt7932_event{unsigned length;u8 packet[64];};
struct mt7932{
 struct work_struct scan_finish_work,startup_work;
 int scan_timeout_work,command_mutex,response_lock,scan_done,discovery_done;
 struct cfg80211_scan_request*scan_request;
 bool scan_aborted,scan_finished,reg_pending,link_failed,discovering,discovery_finished;
 u8 scan_seq,retired_scan_seq;unsigned reg_generation;
 struct work_struct disconnect_work;
 struct wiphy *wiphy;struct wireless_dev wdev;void *netdev;
 bool disconnect_local,disconnecting;u16 disconnect_reason;
};
static unsigned done,cancel,schedule,completion;static bool was_aborted;
static void cancel_delayed_work_sync(int*p){(void)p;}static void cancel_delayed_work(int*p){(void)p;}
static void mutex_lock(int*p){(void)p;}static void mutex_unlock(int*p){(void)p;}
static void complete(int*p){(void)p;completion++;}
static void schedule_work(struct work_struct*p){(void)p;schedule++;}
static int mt_scan_submit_next(struct mt7932*m){(void)m;return 1;}
static void mt_transport_snapshot(struct mt7932*m){(void)m;}
static int mt_request(struct mt7932*m,u8 cid,bool runtime,bool set,bool wait,const u8*p,unsigned n){
 assert(cid==0x1b&&runtime&&set&&!wait&&n==4&&p[0]==m->scan_seq);cancel++;return 0;
}
static void cfg80211_scan_done(struct cfg80211_scan_request*r,struct cfg80211_scan_info*i){assert(r);done++;was_aborted=i->aborted;}
static struct mt7932 *current;
static int retire,disconnected,natural;
static struct mt7932 *mt_from_wiphy(struct wiphy*w){(void)w;return current;}
static void mt_scan_finish_work(struct work_struct*w);
static void flush_work(struct work_struct*w){mt_scan_finish_work(w);}
static unsigned msecs_to_jiffies(unsigned n){return n;}
static int wait_for_completion_timeout(int*c,unsigned n){
 (void)c;assert(n==5000);
 if(natural){current->scan_finished=true;mt_scan_finish_work(&current->scan_finish_work);return 1;}
 return 0;
}
static int mt_retire_connection(struct mt7932*m){assert(!m->scan_request&&!m->retired_scan_seq);retire++;return 0;}
static void cfg80211_disconnected(void*n,u16 r,void*i,unsigned len,bool local,int gfp){
 (void)n;(void)r;(void)i;(void)len;(void)local;(void)gfp;disconnected++;
}
'''
main = r'''
int main(void){
 struct cfg80211_scan_request r={0};struct mt7932 m={.scan_request=&r,.scan_seq=7,.scan_aborted=true};
 mt_scan_finish_work(&m.scan_finish_work);assert(done==1&&cancel==1&&was_aborted&&m.retired_scan_seq==7&&!m.scan_request&&!m.link_failed);
 mt_scan_finish_work(&m.scan_finish_work);assert(done==1&&cancel==1);
 struct mt7932_event e={.length=36};e.packet[36]=7;mt_scan_event(&m,&e);assert(m.retired_scan_seq==7&&!completion);
 e.length=37;e.packet[36]=8;mt_scan_event(&m,&e);assert(m.retired_scan_seq==7&&!completion);
 m.reg_pending=true;m.scan_request=&r;m.scan_seq=8;e.packet[36]=7;
 mt_scan_event(&m,&e);assert(!m.retired_scan_seq&&completion==1&&schedule==1&&m.reg_generation==1&&!m.scan_finished&&done==1);
 mt_scan_event(&m,&e);assert(completion==1&&schedule==1&&done==1);
 e.packet[36]=8;mt_scan_event(&m,&e);assert(m.scan_finished&&completion==2&&schedule==2);
 mt_scan_finish_work(&m.scan_finish_work);assert(done==2&&cancel==1&&!m.retired_scan_seq&&was_aborted);
 m.reg_pending=false;m.scan_request=&r;m.scan_seq=9;m.scan_finished=true;m.scan_aborted=false;
 mt_scan_finish_work(&m.scan_finish_work);assert(done==3&&cancel==1&&!was_aborted);
 current=&m;natural=1;m.scan_request=&r;m.scan_finished=false;m.disconnecting=true;
 mt_disconnect_work(&m.disconnect_work);
 assert(retire==1&&disconnected==1&&done==4&&!m.disconnecting&&!m.link_failed);
 natural=0;m.scan_request=&r;m.scan_finished=false;m.scan_seq=10;m.disconnecting=true;
 mt_disconnect_work(&m.disconnect_work);
 assert(retire==1&&disconnected==2&&done==5&&m.link_failed&&m.retired_scan_seq==10);
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-scan-lifecycle-') as tmp:
    c, exe = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + finish + event + quiesce + disconnect + main)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    str(c), '-o', str(exe)], check=True)
    subprocess.run([str(exe)], check=True)
print('Actual scan lifecycle: single local abort completion, no invented ACK, late/mismatched/truncated events, old sequence cannot finish new scan, natural completion PASS')

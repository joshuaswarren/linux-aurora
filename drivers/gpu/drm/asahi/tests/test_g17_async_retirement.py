#!/usr/bin/env python3
"""Run production per-QID completion/retirement methods against firmware witnesses."""
from pathlib import Path
import subprocess,tempfile
p=Path(__file__).resolve().parents[1]
def item(s,needle):
 a=s.index(needle);start=s.index('{',a);depth=1;i=start+1
 while depth:
  depth+=(s[i]=='{')-(s[i]=='}');i+=1
 return s[a:i]
m=(p/'g17_manager.rs').read_text();r=(p/'g17_async_compute.rs').read_text();l=(p/'g17_live_boot.rs').read_text()
mask_helper=item(r,'const fn g17p_async_completion_mask_contains(')
methods='\n'.join(item(r,n) for n in [
 '    fn service_async_compute_completions_for_event(',
 '    fn service_async_compute_completions(',
 '    fn service_async_compute_completions_matching(',
 '    pub(crate) fn release_async_compute_queue(',
 '    pub(crate) fn quarantine_async_compute('])
publish = item(r, '    pub(crate) fn publish_async_compute(')
service = item(r, '    fn service_async_compute_completions_matching(')
assert 'queue_g17p_firmware_event_worker' not in publish
assert 'fsleep(' not in service
assert 'async_visibility_pending' not in service
dispatch=item(l, '    pub(crate) fn service_primary_firmware_events(')
assert 'if completion_masks == [0; 2]' in dispatch
assert 'self.service_async_compute_completions_for_event(completion_masks)' in dispatch
quarantine = item(r, '    pub(crate) fn quarantine_async_compute(')
assert quarantine.index('self.service_async_compute_completions()') < quarantine.index('q.quarantined = true')
source=r'''
#![allow(dead_code,unused_variables)]
use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
type Error=i32;type Result<T=()>=std::result::Result<T,Error>;
const EIO:i32=5;const EINVAL:i32=22;const EAGAIN:i32=11;const ENODEV:i32=19;
macro_rules! dev_info{($($t:tt)*)=>{()}}
macro_rules! dev_err{($($t:tt)*)=>{()}}
struct Delta;impl Delta{fn from_micros(_:u64)->Self{Self}}fn fsleep(_:Delta){}
mod util { pub fn g17p_verbose() -> bool { false } }
mod g17_submission {pub fn complete_g17p_cl_kicks(p:u32,n:u32)->Result<u32,()>{p.checked_sub(n).ok_or(())}}
thread_local! { static POLLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
struct Resources{records:Vec<(u64,[u64;2])>}
impl Resources{fn read_compute_execution_interval(&mut self,d:u64,end:u64)->Result<Option<[u64;2]>>{Ok(self.records.iter().find(|(desc,t)|*desc==d&&t[1]==end).map(|(_,t)|*t))}fn read_compute_completion(&mut self,descriptor:u64,last:u64)->Result<Option<[u64;2]>>{POLLS.with(|p|p.set(p.get()+1));Ok(self.records.iter().find(|(d,t)|*d==descriptor&&t[1]>last).map(|(_,t)|*t))}}
struct Stamp(u64);impl Stamp{fn read_compute_stamp(&self)->Result<u64>{Ok(self.0)}}
struct G17PSksmQueue{queue_id:u8,independent_context:bool,completion_descriptor:u64,completion_last_end:u64,completion_stamp_armed:bool,compute:Stamp,producer:u32}
impl G17PSksmQueue{fn queue_id(&self)->u8{self.queue_id}}
struct Manager{resources:Resources}
thread_local! { static TIMEOUTS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) }; }
impl Manager {
 fn log_async_compute_timeout(&mut self,q:&mut G17PSksmQueue){TIMEOUTS.with(|t|t.borrow_mut().push(q.completion_descriptor));}
 fn dump_compute_completion_records(&mut self,_label:&str)->Result{Ok(())}
'''+item(m,'    pub(crate) fn poll_independent_compute_completion(')+r'''
}
struct Completion{signals:AtomicUsize,error:AtomicUsize}
impl Completion{fn complete(&self,r:Result<[u64;2]>){self.signals.fetch_add(1,Ordering::SeqCst);if r.is_err(){self.error.fetch_add(1,Ordering::SeqCst);}}fn fail_while_owned(&self,e:Error){self.complete(Err(e));}}
struct Packet{id:u64,completion:Arc<Completion>}
impl Packet{fn submission_id(&self)->u64{self.id}fn completion(&self)->Result<&Completion>{Ok(&self.completion)}}
struct Hold(Arc<AtomicUsize>);impl Hold{fn quarantine(&self){}}impl Drop for Hold{fn drop(&mut self){self.0.fetch_add(1,Ordering::SeqCst);}}
struct Active{packet:Packet,_lease:Hold}
struct Queue{owner:Option<u64>,queue:G17PSksmQueue,binding:Option<Hold>,context:Option<Hold>,active:Option<Active>,released:bool,quarantined:bool}
struct G17PLiveRuntime{pdev:Option<()>,manager:Option<Manager>,async_compute_queues:Vec<Queue>,session_recovery_pending:bool}
'''+mask_helper+r'''
impl G17PLiveRuntime {
'''+methods+r'''
}
fn q(owner:u64,descriptor:u64,d:&Arc<AtomicUsize>)->(Queue,Arc<Completion>){let c=Arc::new(Completion{signals:AtomicUsize::new(0),error:AtomicUsize::new(0)});(Queue{owner:Some(owner),queue:G17PSksmQueue{queue_id:owner as u8,independent_context:true,completion_descriptor:descriptor,completion_last_end:0,completion_stamp_armed:true,compute:Stamp(0),producer:1},binding:Some(Hold(d.clone())),context:Some(Hold(d.clone())),active:Some(Active{packet:Packet{id:owner,completion:c.clone()},_lease:Hold(d.clone())}),released:false,quarantined:false},c)}
#[test]fn out_of_order_requires_matching_descriptor_and_final_visibility(){
 let drops=Arc::new(AtomicUsize::new(0));let(a,ca)=q(1,0x100,&drops);let(b,cb)=q(2,0x200,&drops);
 let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![(0x200,[10,20])]}}),async_compute_queues:vec![a,b],session_recovery_pending:false};
 r.service_async_compute_completions().unwrap();assert_eq!(ca.signals.load(Ordering::SeqCst),0);assert_eq!(cb.signals.load(Ordering::SeqCst),0);assert_eq!(drops.load(Ordering::SeqCst),0);
 r.async_compute_queues[1].queue.compute.0=7;r.service_async_compute_completions().unwrap();assert_eq!(cb.signals.load(Ordering::SeqCst),1);assert_eq!(ca.signals.load(Ordering::SeqCst),0);assert_eq!(r.async_compute_queues[1].queue.producer,0);assert_eq!(drops.load(Ordering::SeqCst),1);
 r.service_async_compute_completions().unwrap();assert_eq!(cb.signals.load(Ordering::SeqCst),1);
 r.manager.as_mut().unwrap().resources.records.push((0x100,[5,25]));r.async_compute_queues[0].queue.compute.0=8;r.service_async_compute_completions().unwrap();assert_eq!(ca.signals.load(Ordering::SeqCst),1);assert_eq!(drops.load(Ordering::SeqCst),2);
}
#[test]fn close_during_execution_defers_context_and_mapping_release(){
 let d=Arc::new(AtomicUsize::new(0));let(q,c)=q(1,0x100,&d);let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![]}}),async_compute_queues:vec![q],session_recovery_pending:false};
 r.release_async_compute_queue(1);assert_eq!(d.load(Ordering::SeqCst),0);assert_eq!(r.async_compute_queues[0].owner,Some(1));
 r.manager.as_mut().unwrap().resources.records.push((0x100,[2,3]));r.async_compute_queues[0].queue.compute.0=1;r.service_async_compute_completions().unwrap();assert_eq!(c.signals.load(Ordering::SeqCst),1);assert_eq!(d.load(Ordering::SeqCst),3);assert_eq!(r.async_compute_queues[0].owner,None);
}
#[test]fn timeout_and_zero_duration_quarantine_until_proven_stop(){
 let d=Arc::new(AtomicUsize::new(0));let(a,ca)=q(1,0x100,&d);let(b,cb)=q(2,0x200,&d);let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![(0x200,[8,8])]}}),async_compute_queues:vec![a,b],session_recovery_pending:false};
 r.quarantine_async_compute(1,999,110);assert_eq!(ca.signals.load(Ordering::SeqCst),0);r.quarantine_async_compute(1,1,110);r.quarantine_async_compute(1,1,110);assert_eq!(ca.error.load(Ordering::SeqCst),1);
 r.service_async_compute_completions().unwrap();assert_eq!(cb.error.load(Ordering::SeqCst),1);assert!(r.session_recovery_pending);r.release_async_compute_queue(1);r.release_async_compute_queue(2);assert_eq!(d.load(Ordering::SeqCst),0);assert!(r.async_compute_queues.iter().all(|q|q.active.is_some()&&q.quarantined));
 r.async_compute_queues.clear();assert_eq!(d.load(Ordering::SeqCst),6);
}
'''
source += r'''
fn qid_mask(qid:u8)->[u64;2]{let mut mask=[0;2];mask[qid as usize/64]|=1u64<<(qid as usize%64);mask}
#[test]fn completion_mask_selects_every_exact_qid_and_rejects_out_of_range(){
 for qid in 0u8..=127 {
  let mask=qid_mask(qid);
  for candidate in 0u8..=127 {assert_eq!(g17p_async_completion_mask_contains(mask,candidate),candidate==qid);}
 }
 assert!(!g17p_async_completion_mask_contains([u64::MAX;2],128));
 assert!(!g17p_async_completion_mask_contains([u64::MAX;2],255));
}
#[test]fn type1_event_polls_only_its_low_or_high_qid(){
 let d=Arc::new(AtomicUsize::new(0));let(a,ca)=q(4,0x400,&d);let(b,cb)=q(65,0x650,&d);
 let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![(0x400,[2,8]),(0x650,[3,9])]}}),async_compute_queues:vec![a,b],session_recovery_pending:false};
 r.async_compute_queues[0].queue.compute.0=1;r.async_compute_queues[1].queue.compute.0=1;
 POLLS.with(|p|p.set(0));r.service_async_compute_completions_for_event(qid_mask(65)).unwrap();
 assert_eq!(POLLS.with(|p|p.get()),1);assert_eq!(ca.signals.load(Ordering::SeqCst),0);assert_eq!(cb.signals.load(Ordering::SeqCst),1);
 r.service_async_compute_completions_for_event(qid_mask(4)).unwrap();assert_eq!(ca.signals.load(Ordering::SeqCst),1);
 assert!(!g17p_async_completion_mask_contains([u64::MAX;2],200));
}
#[test]fn coalesced_two_client_event_retires_both_and_visibility_retry_stays_armed(){
 let d=Arc::new(AtomicUsize::new(0));let(a,ca)=q(4,0x400,&d);let(b,cb)=q(5,0x500,&d);
 let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![(0x400,[2,8]),(0x500,[3,9])]}}),async_compute_queues:vec![a,b],session_recovery_pending:false};
 r.async_compute_queues[0].queue.compute.0=1;
 let mut both=qid_mask(4);both[0]|=qid_mask(5)[0];
 r.service_async_compute_completions_for_event(both).unwrap();
 assert_eq!(ca.signals.load(Ordering::SeqCst),1);assert_eq!(cb.signals.load(Ordering::SeqCst),0);assert!(r.async_compute_queues[1].active.is_some());
 r.async_compute_queues[1].queue.compute.0=1;r.service_async_compute_completions_for_event(qid_mask(5)).unwrap();assert_eq!(cb.signals.load(Ordering::SeqCst),1);
}
#[test]fn identityless_watchdog_fallback_still_scans_every_active_queue(){
 let d=Arc::new(AtomicUsize::new(0));let(a,_)=q(4,0x400,&d);let(b,_)=q(65,0x650,&d);
 let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![]}}),async_compute_queues:vec![a,b],session_recovery_pending:false};
 POLLS.with(|p|p.set(0));r.service_async_compute_completions().unwrap();assert_eq!(POLLS.with(|p|p.get()),2);
}
#[test]fn full_active_capacity_event_does_one_ring_scan_not_fifty_eight(){
 let d=Arc::new(AtomicUsize::new(0));let mut queues=Vec::new();
 for qid in 4u8..62 {queues.push(q(qid as u64,(qid as u64)<<12,&d).0);}
 let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![]}}),async_compute_queues:queues,session_recovery_pending:false};
 POLLS.with(|p|p.set(0));r.service_async_compute_completions_for_event(qid_mask(37)).unwrap();assert_eq!(POLLS.with(|p|p.get()),1);
 POLLS.with(|p|p.set(0));r.service_async_compute_completions().unwrap();assert_eq!(POLLS.with(|p|p.get()),58);
}
#[test]fn early_notification_remains_armed_until_later_record(){
 let d=Arc::new(AtomicUsize::new(0));let(q,c)=q(1,0x100,&d);let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![]}}),async_compute_queues:vec![q],session_recovery_pending:false};
 r.service_async_compute_completions().unwrap();assert_eq!(c.signals.load(Ordering::SeqCst),0);
 r.manager.as_mut().unwrap().resources.records.push((0x100,[2,8]));r.async_compute_queues[0].queue.compute.0=1;
 r.service_async_compute_completions().unwrap();assert_eq!(c.signals.load(Ordering::SeqCst),1);
}
#[test]fn timeout_retires_already_completed_work_without_quarantine(){
 let d=Arc::new(AtomicUsize::new(0));let(q,c)=q(1,0x100,&d);let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![(0x100,[2,8])]}}),async_compute_queues:vec![q],session_recovery_pending:false};
 r.async_compute_queues[0].queue.compute.0=1;
 assert!(!r.quarantine_async_compute(1,1,110));assert!(!r.async_compute_queues[0].quarantined);assert!(!r.session_recovery_pending);assert_eq!(c.signals.load(Ordering::SeqCst),1);assert_eq!(c.error.load(Ordering::SeqCst),0);
}
#[test]fn timeout_diagnostics_select_exact_owner_once_after_last_retirement_attempt(){
 TIMEOUTS.with(|t|t.borrow_mut().clear());
 let d=Arc::new(AtomicUsize::new(0));let(a,_)=q(4,0x400,&d);let(b,_)=q(7,0x700,&d);
 let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![(0x400,[2,8])]}}),async_compute_queues:vec![a,b],session_recovery_pending:false};
 r.async_compute_queues[0].queue.compute.0=1;
 assert!(!r.quarantine_async_compute(7,4,110)); // stale/foreign submission
 assert!(!r.quarantine_async_compute(4,4,110)); // successful final DRAM poll
 assert!(r.quarantine_async_compute(7,7,110));
 assert!(r.quarantine_async_compute(7,7,110)); // duplicate timer, no second dump
 TIMEOUTS.with(|t|assert_eq!(*t.borrow(),vec![0x700]));
 assert_eq!(d.load(Ordering::SeqCst),1); // only the completed QID4 packet dropped
 assert!(r.async_compute_queues[1].active.is_some());
}
'''
with tempfile.TemporaryDirectory() as d:
 f=Path(d)/'test.rs';f.write_text(source);exe=Path(d)/'test'
 subprocess.run(['rustc','--edition=2021','--test',str(f),'-o',str(exe)],check=True)
 subprocess.run([str(exe),'--test-threads=1'],check=True)
print('PASS production async retirement: per-descriptor routing, final stamp, out-of-order completion, close during execution, timeout/invalid timestamp quarantine; no hardware claim')

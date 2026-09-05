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
m=(p/'g17_manager.rs').read_text();r=(p/'g17_async_compute.rs').read_text()
methods='\n'.join(item(r,n) for n in ['    fn service_async_compute_completions(', '    pub(crate) fn release_async_compute_queue(', '    pub(crate) fn quarantine_async_compute('])
source=r'''
#![allow(dead_code,unused_variables)]
use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
type Error=i32;type Result<T=()>=std::result::Result<T,Error>;
const EIO:i32=5;const EINVAL:i32=22;const EAGAIN:i32=11;const ENODEV:i32=19;
macro_rules! dev_info{($($t:tt)*)=>{()}}
macro_rules! dev_err{($($t:tt)*)=>{()}}
struct Delta;impl Delta{fn from_micros(_:u64)->Self{Self}}fn fsleep(_:Delta){}
mod g17_submission {pub fn complete_g17p_cl_kicks(p:u32,n:u32)->Result<u32,()>{p.checked_sub(n).ok_or(())}}
struct Resources{records:Vec<(u64,[u64;2])>}
impl Resources{fn read_compute_execution_interval(&mut self,d:u64,end:u64)->Result<Option<[u64;2]>>{Ok(self.records.iter().find(|(desc,t)|*desc==d&&t[1]==end).map(|(_,t)|*t))}fn read_compute_completion(&mut self,descriptor:u64,last:u64)->Result<Option<[u64;2]>>{Ok(self.records.iter().find(|(d,t)|*d==descriptor&&t[1]>last).map(|(_,t)|*t))}}
struct Stamp(u64);impl Stamp{fn read_compute_stamp(&self)->Result<u64>{Ok(self.0)}}
struct G17PSksmQueue{independent_context:bool,completion_descriptor:u64,completion_last_end:u64,completion_stamp_armed:bool,compute:Stamp,producer:u32}
struct Manager{resources:Resources}
impl Manager {
 fn dump_compute_completion_records(&mut self,_label:&str)->Result{Ok(())}
'''+item(m,'    pub(crate) fn poll_independent_compute_completion(')+r'''
}
struct Completion{signals:AtomicUsize,error:AtomicUsize}
impl Completion{fn complete(&self,r:Result<[u64;2]>){self.signals.fetch_add(1,Ordering::SeqCst);if r.is_err(){self.error.fetch_add(1,Ordering::SeqCst);}}}
struct Packet{id:u64,completion:Arc<Completion>}
impl Packet{fn submission_id(&self)->u64{self.id}fn completion(&self)->Result<&Completion>{Ok(&self.completion)}}
struct Hold(Arc<AtomicUsize>);impl Hold{fn quarantine(&self){}}impl Drop for Hold{fn drop(&mut self){self.0.fetch_add(1,Ordering::SeqCst);}}
struct Active{packet:Packet,_lease:Hold}
struct Queue{owner:Option<u64>,queue:G17PSksmQueue,binding:Option<Hold>,context:Option<Hold>,active:Option<Active>,released:bool,quarantined:bool}
struct G17PLiveRuntime{pdev:Option<()>,manager:Option<Manager>,async_compute_queues:Vec<Queue>,async_visibility_pending:bool}
impl G17PLiveRuntime {
'''+methods+r'''
}
fn q(owner:u64,descriptor:u64,d:&Arc<AtomicUsize>)->(Queue,Arc<Completion>){let c=Arc::new(Completion{signals:AtomicUsize::new(0),error:AtomicUsize::new(0)});(Queue{owner:Some(owner),queue:G17PSksmQueue{independent_context:true,completion_descriptor:descriptor,completion_last_end:0,completion_stamp_armed:true,compute:Stamp(0),producer:1},binding:Some(Hold(d.clone())),context:Some(Hold(d.clone())),active:Some(Active{packet:Packet{id:owner,completion:c.clone()},_lease:Hold(d.clone())}),released:false,quarantined:false},c)}
#[test]fn out_of_order_requires_matching_descriptor_and_final_visibility(){
 let drops=Arc::new(AtomicUsize::new(0));let(a,ca)=q(1,0x100,&drops);let(b,cb)=q(2,0x200,&drops);
 let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![(0x200,[10,20])]}}),async_compute_queues:vec![a,b],async_visibility_pending:false};
 r.service_async_compute_completions().unwrap();assert!(r.async_visibility_pending);assert_eq!(ca.signals.load(Ordering::SeqCst),0);assert_eq!(cb.signals.load(Ordering::SeqCst),0);assert_eq!(drops.load(Ordering::SeqCst),0);
 r.async_compute_queues[1].queue.compute.0=7;r.service_async_compute_completions().unwrap();assert_eq!(cb.signals.load(Ordering::SeqCst),1);assert_eq!(ca.signals.load(Ordering::SeqCst),0);assert_eq!(r.async_compute_queues[1].queue.producer,0);assert_eq!(drops.load(Ordering::SeqCst),1);
 r.service_async_compute_completions().unwrap();assert_eq!(cb.signals.load(Ordering::SeqCst),1);
 r.manager.as_mut().unwrap().resources.records.push((0x100,[5,25]));r.async_compute_queues[0].queue.compute.0=8;r.service_async_compute_completions().unwrap();assert_eq!(ca.signals.load(Ordering::SeqCst),1);assert_eq!(drops.load(Ordering::SeqCst),2);
}
#[test]fn close_during_execution_defers_context_and_mapping_release(){
 let d=Arc::new(AtomicUsize::new(0));let(q,c)=q(1,0x100,&d);let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![]}}),async_compute_queues:vec![q],async_visibility_pending:false};
 r.release_async_compute_queue(1);assert_eq!(d.load(Ordering::SeqCst),0);assert_eq!(r.async_compute_queues[0].owner,Some(1));
 r.manager.as_mut().unwrap().resources.records.push((0x100,[2,3]));r.async_compute_queues[0].queue.compute.0=1;r.service_async_compute_completions().unwrap();assert_eq!(c.signals.load(Ordering::SeqCst),1);assert_eq!(d.load(Ordering::SeqCst),3);assert_eq!(r.async_compute_queues[0].owner,None);
}
#[test]fn timeout_and_zero_duration_quarantine_until_proven_stop(){
 let d=Arc::new(AtomicUsize::new(0));let(a,ca)=q(1,0x100,&d);let(b,cb)=q(2,0x200,&d);let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![(0x200,[8,8])]}}),async_compute_queues:vec![a,b],async_visibility_pending:false};
 r.quarantine_async_compute(1,999,110);assert_eq!(ca.signals.load(Ordering::SeqCst),0);r.quarantine_async_compute(1,1,110);r.quarantine_async_compute(1,1,110);assert_eq!(ca.error.load(Ordering::SeqCst),1);
 r.service_async_compute_completions().unwrap();assert_eq!(cb.error.load(Ordering::SeqCst),1);r.release_async_compute_queue(1);r.release_async_compute_queue(2);assert_eq!(d.load(Ordering::SeqCst),0);assert!(r.async_compute_queues.iter().all(|q|q.active.is_some()&&q.quarantined));
 r.async_compute_queues.clear();assert_eq!(d.load(Ordering::SeqCst),6);
}
'''
source += r'''
#[test]fn early_notification_remains_armed_until_later_record(){
 let d=Arc::new(AtomicUsize::new(0));let(q,c)=q(1,0x100,&d);let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![]}}),async_compute_queues:vec![q],async_visibility_pending:false};
 r.service_async_compute_completions().unwrap();assert!(r.async_visibility_pending);assert_eq!(c.signals.load(Ordering::SeqCst),0);
 r.manager.as_mut().unwrap().resources.records.push((0x100,[2,8]));r.async_compute_queues[0].queue.compute.0=1;
 r.service_async_compute_completions().unwrap();assert!(!r.async_visibility_pending);assert_eq!(c.signals.load(Ordering::SeqCst),1);
}
#[test]fn timeout_retires_already_completed_work_without_quarantine(){
 let d=Arc::new(AtomicUsize::new(0));let(q,c)=q(1,0x100,&d);let mut r=G17PLiveRuntime{pdev:None,manager:Some(Manager{resources:Resources{records:vec![(0x100,[2,8])]}}),async_compute_queues:vec![q],async_visibility_pending:false};
 r.async_compute_queues[0].queue.compute.0=1;
 assert!(!r.quarantine_async_compute(1,1,110));assert!(!r.async_compute_queues[0].quarantined);assert_eq!(c.signals.load(Ordering::SeqCst),1);assert_eq!(c.error.load(Ordering::SeqCst),0);
}
'''
with tempfile.TemporaryDirectory() as d:
 f=Path(d)/'test.rs';f.write_text(source);exe=Path(d)/'test'
 subprocess.run(['rustc','--edition=2021','--test',str(f),'-o',str(exe)],check=True)
 subprocess.run([str(exe),'--test-threads=1'],check=True)
print('PASS production async retirement: per-descriptor routing, final stamp, out-of-order completion, close during execution, timeout/invalid timestamp quarantine; no hardware claim')

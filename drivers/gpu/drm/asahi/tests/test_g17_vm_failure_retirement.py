#!/usr/bin/env python3
"""Run production VM failure/retirement methods against observable owners.

No hardware, kernel callbacks, fault reproduction or unsafe pointer access.
The firmware predicate is an explicit mock witness; its detailed validation is
covered by test_g17_async_retirement.py. This test covers routing into that
predicate, exact ticket identity, retained pins, and once-only notification.
Set NEO_FANOUT_SOURCE to another driver directory for the negative control.
"""
import os
from pathlib import Path
import subprocess
import tempfile

from test_g17_compute_scs_encoding import block
from g17_compute_inflight_test_support import completion_mod, queue_limits_mock

ROOT = Path(__file__).resolve().parents[1]
SOURCE = Path(os.environ.get("NEO_FANOUT_SOURCE", ROOT))
compute = (SOURCE / "g17_async_compute.rs").read_text()
job = (SOURCE / "g17_job.rs").read_text()
methods = "\n".join(block(compute, marker) for marker in [
    "    fn fail_outstanding_jobs_for_vm_after_compute_observation(",
    "    pub(crate) fn fail_outstanding_jobs_for_vm(",
    "    fn release_firmware_retired_quarantined_compute(",
])
completion_methods = "\n".join(block(job, marker) for marker in [
    "    pub(crate) fn fail_while_owned(",
    "    pub(crate) fn release_after_retirement(",
])

source = r'''
#![allow(dead_code,unused_variables)]
use std::sync::{Arc,Mutex as StdMutex};
use std::sync::atomic::{AtomicI32,AtomicU32,AtomicUsize,Ordering};
#[derive(Debug,Copy,Clone,PartialEq,Eq)] struct Error(i32);
impl Error {fn to_errno(self)->i32{self.0}}
const EIO:Error=Error(-5);
type Result<T=()> = std::result::Result<T,Error>;
macro_rules! dev_info {($($t:tt)*)=>{()}}
macro_rules! dev_err {($($t:tt)*)=>{()}}
struct Mutex<T>(StdMutex<T>);
impl<T> Mutex<T>{fn lock(&self)->std::sync::MutexGuard<'_,T>{self.0.lock().unwrap()}}
mod g17_status {
 use super::*;
 #[derive(Default)] pub struct VmStatus(pub AtomicI32);
 impl VmStatus {pub fn record(&self,e:i32){let _=self.0.compare_exchange(0,e,Ordering::AcqRel,Ordering::Acquire);}}
}
#[derive(Default)] struct Counts {pins:AtomicUsize,graphs_dropped:AtomicUsize,leases_dropped:AtomicUsize}
struct Pin(Arc<Counts>);
impl Pin {fn quarantine(&self){}}
impl Drop for Pin {fn drop(&mut self){self.0.pins.fetch_sub(1,Ordering::SeqCst);}}
struct Graph(Arc<Counts>);
impl Drop for Graph {fn drop(&mut self){self.0.graphs_dropped.fetch_add(1,Ordering::SeqCst);}}
#[derive(Default)] struct Fence {signals:AtomicUsize,error:AtomicI32}
impl Fence {
 fn set_error(&self,e:Error){self.error.store(e.0,Ordering::SeqCst);}
 fn signal(&self){self.signals.fetch_add(1,Ordering::SeqCst);}
}
#[derive(Default)] struct Aggregate(AtomicUsize);
impl Aggregate {fn complete(&self,_:Result){self.0.fetch_add(1,Ordering::SeqCst);}}
struct Completion {signalled:AtomicU32,vm_job:Mutex<Option<Pin>>,status:Arc<g17_status::VmStatus>,fence:Fence,aggregate_member:Aggregate}
impl Completion {
''' + completion_methods + r'''
}
struct Context(u32);
impl Context {fn context_id(&self)->u32{self.0}}
struct Packet {id:u64,context:Context,completion:Arc<Completion>}
impl Packet {
 fn submission_id(&self)->u64{self.id}
 fn context(&self)->&Context{&self.context}
 fn completion(&self)->Result<&Completion>{Ok(&self.completion)}
}
struct Lease(Arc<Counts>);
impl Lease {fn quarantine(&self){}}
impl Drop for Lease {fn drop(&mut self){self.0.leases_dropped.fetch_add(1,Ordering::SeqCst);}}
#[derive(Clone,Copy)] struct FirmwareTicket(u64);
#[derive(Clone,Copy)] struct Ticket {owner:u64,submission:u64,context:u32,firmware:FirmwareTicket}
struct Active {packet:Arc<Packet>,_lease:Lease,retirement:Option<Ticket>}
''' + completion_mod() + queue_limits_mock() + r'''
type Inflight=g17_completion::PairFifo<Active,{g17_queue_limits::MAX_COMPUTE_IN_FLIGHT_PER_QUEUE as usize}>;
type RetiredLeases=g17_completion::PairFifo<Lease,{g17_queue_limits::MAX_COMPUTE_IN_FLIGHT_PER_QUEUE as usize}>;
struct HardwareQueue(u8);
impl HardwareQueue {fn queue_id(&self)->u8{self.0}}
struct Queue {owner:Option<u64>,queue:HardwareQueue,inflight:Inflight,quarantined:bool,awaiting_retirement_witness:bool,retired_leases:RetiredLeases,_graph:Graph}
struct Device;
impl Device {fn as_ref(&self)->&Self{self}}
mod g17_resources {
 pub struct G17PRenderQueuePair;
 impl G17PRenderQueuePair {pub const SLOT_COUNT:u8=2;}
}
struct Render {status:Arc<g17_status::VmStatus>,failed:AtomicUsize}
impl Render {
 fn belongs_to_vm(&self,s:&Arc<g17_status::VmStatus>)->bool{Arc::ptr_eq(s,&self.status)}
 fn fail_while_owned(&self,_:Error)->bool{self.failed.fetch_add(1,Ordering::SeqCst)==0}
}
struct Handshake {state:u64}
struct Manager {state:u64,proof:Result<bool>,queries:Vec<(u8,u64,u32)>}
impl Manager {
 fn firmware_recovery_handshake_snapshot(&mut self)->Result<Handshake>{Ok(Handshake{state:self.state})}
 fn independent_compute_retired_by_firmware(&mut self,q:&mut HardwareQueue,t:&FirmwareTicket,n:u32)->Result<bool>{
  self.queries.push((q.0,t.0,n));self.proof
 }
}
struct Runtime {async_compute_queues:Vec<Queue>,pending_render:[Option<Render>;2],manager:Option<Manager>,pdev:Option<Device>,scans:usize}
impl Runtime {
 fn service_async_compute_completions(&mut self)->Result{self.scans+=1;Ok(())}
 fn poll_deferred_render_completion(&mut self,_:u8)->Result<Option<()>>{Ok(None)}
 fn settle_deferred_render_completion(&mut self,_:u8,_:())->Result<bool>{unreachable!()}
''' + methods + r'''
}
fn fixture()->(Runtime,Arc<g17_status::VmStatus>,Arc<Counts>,Vec<Arc<Completion>>) {
 let status=Arc::new(g17_status::VmStatus::default());let counts=Arc::new(Counts::default());
 let mut inflight=Inflight::new();let mut completions=Vec::new();
 for id in [100,101] {
  counts.pins.fetch_add(1,Ordering::SeqCst);
  let c=Arc::new(Completion{signalled:AtomicU32::new(0),vm_job:Mutex(StdMutex::new(Some(Pin(counts.clone())))),status:status.clone(),fence:Fence::default(),aggregate_member:Aggregate::default()});
  completions.push(c.clone());
  let active=Active{packet:Arc::new(Packet{id,context:Context(10),completion:c}),_lease:Lease(counts.clone()),retirement:Some(Ticket{owner:65,submission:id,context:10,firmware:FirmwareTicket(id)})};
  assert!(inflight.push(active).is_ok());
 }
 let q=Queue{owner:Some(65),queue:HardwareQueue(13),inflight,quarantined:false,awaiting_retirement_witness:false,retired_leases:RetiredLeases::new(),_graph:Graph(counts.clone())};
 (Runtime{async_compute_queues:vec![q],pending_render:[None,None],manager:Some(Manager{state:0,proof:Ok(false),queries:Vec::new()}),pdev:None,scans:0},status,counts,completions)
}
fn check_retained(rt:&Runtime,c:&Counts){
 assert_eq!(rt.async_compute_queues[0].inflight.len(),2);
 assert_eq!(c.pins.load(Ordering::SeqCst),2);
 assert_eq!(c.graphs_dropped.load(Ordering::SeqCst),0);
 assert_eq!(c.leases_dropped.load(Ordering::SeqCst),0);
}
#[test] fn sibling_loss_arms_the_existing_witness_without_releasing_pins(){
 let(mut rt,status,c,completions)=fixture();
 assert_eq!(rt.fail_outstanding_jobs_for_vm(&status,EIO),2);
 assert!(rt.async_compute_queues[0].quarantined);
 assert!(rt.async_compute_queues[0].awaiting_retirement_witness,"VM fanout stranded ticketed owners outside retirement observation");
 assert_eq!(rt.scans,1);assert!(rt.manager.as_ref().unwrap().queries.is_empty());
 check_retained(&rt,&c);
 for completion in completions {assert_eq!(completion.fence.signals.load(Ordering::SeqCst),1);assert_eq!(completion.fence.error.load(Ordering::SeqCst),-5);}
}
#[test] fn later_exact_retirement_releases_pins_even_with_scheduler_references(){
 let(mut rt,status,c,completions)=fixture();
 rt.fail_outstanding_jobs_for_vm(&status,EIO);
 rt.manager.as_mut().unwrap().proof=Ok(true);
 rt.release_firmware_retired_quarantined_compute("test");
 assert!(rt.async_compute_queues[0].inflight.is_none(),"ticketed failed packets stayed pinned after proof");
 assert!(!rt.async_compute_queues[0].awaiting_retirement_witness);
 assert!(rt.async_compute_queues[0].quarantined);
 assert_eq!(rt.async_compute_queues[0].retired_leases.len(),2);
 assert_eq!(c.pins.load(Ordering::SeqCst),0);
 assert_eq!(c.graphs_dropped.load(Ordering::SeqCst),0);
 assert_eq!(c.leases_dropped.load(Ordering::SeqCst),0);
 assert_eq!(rt.manager.as_ref().unwrap().queries,vec![(13,101,2)]);
 rt.release_firmware_retired_quarantined_compute("again");
 assert_eq!(rt.manager.as_ref().unwrap().queries.len(),1);
 for completion in completions {assert_eq!(completion.fence.signals.load(Ordering::SeqCst),1);assert_eq!(completion.aggregate_member.0.load(Ordering::SeqCst),1);}
 assert_eq!(status.0.load(Ordering::SeqCst),-5);
}
#[test] fn unretired_or_recovering_firmware_keeps_every_owner(){
 let(mut rt,status,c,_)=fixture();rt.fail_outstanding_jobs_for_vm(&status,EIO);
 rt.manager.as_mut().unwrap().state=1;rt.manager.as_mut().unwrap().proof=Ok(true);
 rt.release_firmware_retired_quarantined_compute("recovery");check_retained(&rt,&c);
 assert!(rt.manager.as_ref().unwrap().queries.is_empty());
 rt.manager.as_mut().unwrap().state=0;rt.manager.as_mut().unwrap().proof=Ok(false);
 rt.release_firmware_retired_quarantined_compute("not-retired");check_retained(&rt,&c);
 assert_eq!(rt.manager.as_ref().unwrap().queries.len(),1);
}
#[test] fn incomplete_publication_identity_does_not_arm_a_partial_release(){
 let(mut rt,status,c,_)=fixture();
 rt.async_compute_queues[0].inflight.iter_mut().next().unwrap().retirement=None;
 rt.fail_outstanding_jobs_for_vm(&status,EIO);
 assert!(!rt.async_compute_queues[0].awaiting_retirement_witness);
 rt.manager.as_mut().unwrap().proof=Ok(true);
 rt.release_firmware_retired_quarantined_compute("missing-ticket");check_retained(&rt,&c);
 assert!(rt.manager.as_ref().unwrap().queries.is_empty());
}
#[test] fn mismatched_owner_submission_or_context_never_releases(){
 for field in 0..3 {
  let(mut rt,status,c,_)=fixture();rt.fail_outstanding_jobs_for_vm(&status,EIO);
  let ticket=rt.async_compute_queues[0].inflight.iter_mut().next().unwrap().retirement.as_mut().unwrap();
  match field {0=>ticket.owner+=1,1=>ticket.submission+=1,_=>ticket.context+=1}
  rt.manager.as_mut().unwrap().proof=Ok(true);
  rt.release_firmware_retired_quarantined_compute("wrong-identity");check_retained(&rt,&c);
  assert!(rt.manager.as_ref().unwrap().queries.is_empty());
  assert!(!rt.async_compute_queues[0].awaiting_retirement_witness);
 }
}
#[test] fn witness_errors_fail_closed_without_dropping_packets(){
 let(mut rt,status,c,_)=fixture();rt.fail_outstanding_jobs_for_vm(&status,EIO);
 rt.manager.as_mut().unwrap().proof=Err(EIO);
 rt.release_firmware_retired_quarantined_compute("error");check_retained(&rt,&c);
 assert_eq!(rt.manager.as_ref().unwrap().queries.len(),1);
 assert!(!rt.async_compute_queues[0].awaiting_retirement_witness);
}
#[test] fn unrelated_vm_and_empty_queues_are_unchanged(){
 let(mut rt,_,c,_)=fixture();let other=Arc::new(g17_status::VmStatus::default());
 assert_eq!(rt.fail_outstanding_jobs_for_vm(&other,EIO),0);check_retained(&rt,&c);
 assert!(!rt.async_compute_queues[0].quarantined);
 assert!(!rt.async_compute_queues[0].awaiting_retirement_witness);
 let empty=Queue{owner:None,queue:HardwareQueue(14),inflight:Inflight::new(),quarantined:false,awaiting_retirement_witness:false,retired_leases:RetiredLeases::new(),_graph:Graph(c.clone())};
 rt.async_compute_queues.push(empty);
 assert_eq!(rt.fail_outstanding_jobs_for_vm(&other,EIO),0);
 assert!(!rt.async_compute_queues[1].awaiting_retirement_witness);
}
#[test] fn duplicate_failure_does_not_reset_the_witness_or_signal_twice(){
 let(mut rt,status,c,completions)=fixture();rt.fail_outstanding_jobs_for_vm(&status,EIO);
 assert!(rt.async_compute_queues[0].awaiting_retirement_witness);
 assert_eq!(rt.fail_outstanding_jobs_for_vm(&status,EIO),0);check_retained(&rt,&c);
 assert!(rt.async_compute_queues[0].awaiting_retirement_witness);
 for completion in completions {assert_eq!(completion.fence.signals.load(Ordering::SeqCst),1);}
}
'''

with tempfile.TemporaryDirectory() as directory:
    directory = Path(directory)
    test = directory / "test.rs"
    test.write_text(source)
    executable = directory / "test"
    subprocess.run(["rustc", "--edition=2021", "--test", str(test), "-o", str(executable)], check=True)
    subprocess.run([str(executable), "--test-threads=1"], check=True, timeout=60)

#!/usr/bin/env python3
"""Run the actual counted aggregate with fake fences; no GPU or wait thread."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = (root / 'g17_drm.rs').read_text()


def block(marker):
    start = source.index(marker)
    end = source.index('{', start) + 1
    depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


preamble = r'''
#![allow(dead_code)]
use std::sync::atomic::{AtomicU32,AtomicI32,Ordering};
#[derive(Clone,Copy,Debug,PartialEq)]struct Error(i32);
impl Error {fn to_errno(self)->i32{self.0}fn from_errno(e:i32)->Self{Self(e)}}
type Result<T=()>=std::result::Result<T,Error>;
const ECANCELED:Error=Error(-125);const GFP_KERNEL:()=();
struct Arc<T>(std::sync::Arc<T>);
impl<T>Arc<T>{fn new(value:T,_flags:())->Result<Self>{Ok(Self(std::sync::Arc::new(value)))}}
impl<T>Clone for Arc<T>{fn clone(&self)->Self{Self(self.0.clone())}}
impl<T>std::ops::Deref for Arc<T>{type Target=T;fn deref(&self)->&T{&self.0}}
#[derive(Default)]struct Signals{signals:AtomicU32,error:AtomicI32}
struct UserFence<T>{state:std::sync::Arc<Signals>,_p:std::marker::PhantomData<T>}
impl<T>UserFence<T>{
 fn set_error(&self,error:Error){self.state.error.store(error.0,Ordering::Release);}
 fn signal(&self){self.state.signals.fetch_add(1,Ordering::AcqRel);}
}
struct Fence(std::sync::Arc<Signals>);
impl Fence{fn from_fence<T>(f:&UserFence<T>)->Self{Self(f.state.clone())}}
struct G17PSubmissionFence;
'''
actual = '\n'.join(block(marker) for marker in [
    'pub(crate) struct G17PSubmitCompletion {', 'impl G17PSubmitCompletion {',
    'pub(crate) struct G17PSubmitCompletionMember {', 'impl G17PSubmitCompletionMember {',
    'impl Drop for G17PSubmitCompletionMember {',
])
tests = r'''
fn aggregate()->Arc<G17PSubmitCompletion>{G17PSubmitCompletion::new(UserFence{
 state:std::sync::Arc::new(Signals::default()),_p:std::marker::PhantomData}).unwrap()}
fn signals(a:&G17PSubmitCompletion)->u32{a.fence.state.signals.load(Ordering::Acquire)}
fn error(a:&G17PSubmitCompletion)->i32{a.fence.state.error.load(Ordering::Acquire)}
#[test]fn all_commands_across_both_engines_must_complete_in_any_order(){
 let a=aggregate();let render_a=G17PSubmitCompletionMember::new(a.clone());
 let render_b=G17PSubmitCompletionMember::new(a.clone());let compute=G17PSubmitCompletionMember::new(a.clone());
 a.finish_enqueue(Ok(()));compute.complete(Ok(()));render_b.complete(Ok(()));assert_eq!(signals(&a),0);
 render_a.complete(Ok(()));assert_eq!(signals(&a),1);assert_eq!(error(&a),0);
 drop((render_a,render_b,compute));assert_eq!(signals(&a),1);
}
#[test]fn completion_before_enqueue_finishes_cannot_signal_early(){
 let a=aggregate();let r=G17PSubmitCompletionMember::new(a.clone());r.complete(Ok(()));
 assert_eq!(signals(&a),0);a.finish_enqueue(Ok(()));assert_eq!(signals(&a),1);
}
#[test]fn earlier_error_survives_later_success(){
 let a=aggregate();let first=G17PSubmitCompletionMember::new(a.clone());let last=G17PSubmitCompletionMember::new(a.clone());
 first.complete(Err(Error(-5)));last.complete(Ok(()));a.finish_enqueue(Ok(()));
 assert_eq!(signals(&a),1);assert_eq!(error(&a),-5);
}
#[test]fn partial_enqueue_failure_covers_queued_work_and_actual_error(){
 let a=aggregate();let queued=G17PSubmitCompletionMember::new(a.clone());
 drop(G17PSubmitCompletionMember::new(a.clone())); // failed construction
 a.finish_enqueue(Err(Error(-12)));assert_eq!(signals(&a),0);
 queued.complete(Ok(()));assert_eq!(signals(&a),1);assert_eq!(error(&a),-12);
}
#[test]fn first_enqueue_failure_does_not_strand_exported_fence(){
 let a=aggregate();drop(G17PSubmitCompletionMember::new(a.clone()));a.finish_enqueue(Err(Error(-12)));
 assert_eq!(signals(&a),1);assert_eq!(error(&a),-12);
}
#[test]fn cancellation_and_duplicate_callback_count_once(){
 let a=aggregate();let r=G17PSubmitCompletionMember::new(a.clone());a.finish_enqueue(Ok(()));
 r.complete(Err(ECANCELED));r.complete(Ok(()));drop(r);assert_eq!(signals(&a),1);assert_eq!(error(&a),-125);
}
#[test]fn concurrent_callbacks_preserve_failure_and_signal_once(){
 let a=aggregate();let members=(0..64).map(|_|G17PSubmitCompletionMember::new(a.clone())).collect::<Vec<_>>();
 a.finish_enqueue(Ok(()));std::thread::scope(|scope|{for (i,m) in members.into_iter().enumerate(){
 scope.spawn(move||m.complete(if i==13{Err(Error(-5))}else{Ok(())}));}});
 assert_eq!(signals(&a),1);assert_eq!(error(&a),-5);
}
'''

# Production callback wiring and failure gate, including both engines.
render = block('impl G17PRenderCompletion {')
assert render.count('self.aggregate_member.complete(') == 3
compute = (root / 'g17_job.rs').read_text()
assert compute.count('self.aggregate_member.complete(') == 2
guard = block("impl Drop for G17POutputFencePublication<'_> {")
assert 'aggregate.finish_enqueue(if self.succeeded { Ok(()) } else { Err(self.failure) });' in guard
dispatch = block('    fn submit_guarded(')
assert dispatch.index('publication.aggregate = Some(aggregate.clone());') < dispatch.index('pending.drain(..)')
assert dispatch.count('G17PSubmitCompletionMember::new(aggregate.clone())') == 2
assert 'publication.publish_async(&aggregate.fence());' in dispatch
assert 'publication.publish_async(&finished);' not in source
with tempfile.TemporaryDirectory() as directory:
    path = Path(directory) / 'aggregate.rs'
    binary = Path(directory) / 'aggregate'
    path.write_text(preamble + actual + tests)
    subprocess.run(['rustc', '--edition=2021', '--test', str(path), '-o', str(binary)], check=True)
    subprocess.run([str(binary), '--test-threads=1'], check=True, timeout=60)

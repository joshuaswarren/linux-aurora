#!/usr/bin/env python3
"""Exercise the production PB ID/token allocator, including changed bindings."""
from pathlib import Path
import re,subprocess,tempfile
root=Path(__file__).resolve().parents[4]
source=(root/'drivers/gpu/drm/asahi/g17/buffer.rs').read_text()
def method(name):
 m=re.search(r'(?m)^    (?:pub\(crate\) )?fn '+name+r'\(',source);assert m,name
 i=source.index('{',m.start());j=i+1;depth=1
 while depth:depth+=(source[j]=='{')-(source[j]=='}');j+=1
 return source[m.start():j]
prefix=r'''
#[derive(Debug,PartialEq,Eq,Copy,Clone)] enum Error { Invalid,Io,NoSpace }
const EINVAL:Error=Error::Invalid;const EIO:Error=Error::Io;
type Result<T=()> = std::result::Result<T,Error>;
macro_rules! enospc_site {($s:expr)=>{Error::NoSpace};}
macro_rules! pr_info {($($t:tt)*)=>{};}
mod g17 {pub mod progress {pub fn sampled(_: &core::sync::atomic::AtomicU64)->Option<u64>{None}}}
const BUFFER_IDS:usize=127;
#[derive(Copy,Clone)] struct BufferId {references:u8,owner:bool,token:u64,bound:Option<(u32,u64)>}
impl BufferId {fn free(self)->bool{self.references==0&&!self.owner}}
struct BufferIds {entries:[BufferId;BUFFER_IDS],returned:[u8;BUFFER_IDS],returned_count:usize,next_token:u64}
impl BufferIds {
fn test_new()->Self{Self{entries:[BufferId{references:0,owner:false,token:0,bound:None};BUFFER_IDS],returned:std::array::from_fn(|i|(BUFFER_IDS-i-1)as u8),returned_count:BUFFER_IDS,next_token:0}}
'''
tests=r'''
}
#[test] fn changed_context_gets_new_token(){
 let mut ids=BufferIds::test_new();let(id,initial)=ids.allocate_render().unwrap();ids.reserve_owner(id).unwrap();ids.release(id).unwrap();
 let(_,a)=ids.acquire_owner_bound(id,8,0x1000).unwrap();assert_eq!(a,initial);ids.release(id).unwrap();
 let(_,again)=ids.acquire_owner_bound(id,8,0x1000).unwrap();assert_eq!(again,a);ids.release(id).unwrap();
 let(_,b)=ids.acquire_owner_bound(id,10,0x1000).unwrap();assert_ne!(b,a);ids.release(id).unwrap();
 let(_,c)=ids.acquire_owner_bound(id,10,0x2000).unwrap();assert_ne!(c,b);ids.release(id).unwrap();
 ids.unreserve_owner(id).unwrap();let(reused,d)=ids.allocate_render().unwrap();assert_eq!(reused,id);assert_ne!(d,c);assert_eq!(ids.entries[id as usize].bound,None);
}
#[test] fn legacy_path_is_unchanged(){
 let mut ids=BufferIds::test_new();let(id,initial)=ids.allocate_render().unwrap();ids.reserve_owner(id).unwrap();
 assert_eq!(ids.acquire_owner(id).unwrap().1,initial);assert_eq!(ids.entries[id as usize].bound,None);
}
#[test] fn failed_preparation_never_reuses_an_old_binding_token(){
 let mut ids=BufferIds::test_new();let(id,_)=ids.allocate_render().unwrap();ids.reserve_owner(id).unwrap();ids.release(id).unwrap();
 let(_,a)=ids.acquire_owner_bound(id,8,0x1000).unwrap();ids.release(id).unwrap();
 let(_,b)=ids.acquire_owner_bound(id,10,0x1000).unwrap();ids.release(id).unwrap();
 let(_,c)=ids.acquire_owner_bound(id,8,0x1000).unwrap();assert_ne!(a,b);assert_ne!(a,c);assert_ne!(b,c);
}
#[test] fn invalid_or_overflowed_claim_does_not_mutate_token(){
 let mut ids=BufferIds::test_new();let before=ids.next_token;
 assert_eq!(ids.acquire_owner_bound(0,8,0x1000),Err(EINVAL));assert_eq!(ids.next_token,before);
 ids.entries[0]=BufferId{references:u8::MAX,owner:true,token:9,bound:Some((1,0x1000))};
 assert_eq!(ids.acquire_owner_bound(0,8,0x1000),Err(Error::NoSpace));assert_eq!(ids.next_token,before);assert_eq!(ids.entries[0].token,9);
}
'''
names=['remove_returned','return_id','fresh_token','allocate_render','reserve_owner','acquire_owner','acquire_owner_bound','release','unreserve_owner']
program=prefix+'\n'.join(method(n) for n in names)+tests
with tempfile.TemporaryDirectory(prefix='pb-token-') as tmp:
 path=Path(tmp)/'test.rs';exe=Path(tmp)/'test';path.write_text(program)
 subprocess.run(['rustc','--edition=2021','--test',str(path),'-o',str(exe)],check=True)
 subprocess.run([str(exe)],check=True)
 # The former unbound owner path must fail the changed-context case.
 control=program.replace('ids.acquire_owner_bound(id,10,0x1000)','ids.acquire_owner(id)')
 assert control!=program;path.write_text(control)
 subprocess.run(['rustc','--edition=2021','--test',str(path),'-o',str(exe)],check=True)
 assert subprocess.run([str(exe)],capture_output=True).returncode!=0
print('Production PB token rebind tests pass; original owner-only path fails the negative control')

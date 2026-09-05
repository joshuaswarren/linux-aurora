#!/usr/bin/env python3
"""Use production arena functions to reproduce and exclude independent-root clashes."""
from pathlib import Path
import re,subprocess,tempfile
p=Path(__file__).resolve().parents[1]
s=(p/'g17_resources.rs').read_text()
names=['G17P_CL_DYNAMIC_LOW_VA_START','G17P_CL_CLIENT_LOW_VA_START','G17P_CL_LOW_VA_END']
constants='\n'.join(re.search(r'(?:pub\(crate\) )?const '+n+r': u64 = [^;]+;',s)[0] for n in names)
def method(name):
 a=s.index('fn '+name+'(');b=s.index('{',a);depth=1;e=b+1
 while depth:depth+=(s[e]=='{')-(s[e]=='}');e+=1
 return s[a:e]
functions='\n'.join(method(n) for n in ['g17p_cl_dynamic_low_va_range','g17p_cl_client_low_va_range'])
# The failed hardware run had an empty kernel-low allocator and retained
# client render state at0x7004000000. Both first-fit allocations chose that
# address from the old common range, before UAT.bind was reached.
tests=r'''
use std::ops::Range;
fn overlap(a:&Range<u64>,b:&Range<u64>)->bool{a.start<b.end&&b.start<a.end}
#[test]fn old_independent_allocators_reproduce_fixed_alias_enospc(){
 let old=G17P_CL_DYNAMIC_LOW_VA_START..G17P_CL_LOW_VA_END;
 let retained_render=old.start..old.start+0x34000;
 let canonical_entries=old.start..old.start+0x20000;
 assert!(overlap(&retained_render,&canonical_entries));
}
#[test]fn canonical_and_private_allocations_never_overlap_in_either_order(){
 let global=g17p_cl_dynamic_low_va_range();let client=g17p_cl_client_low_va_range();
 assert!(!overlap(&global,&client));assert!(global.start<global.end&&client.start<client.end);
 for global_size in [0x4000,0x20000,0x80000,global.end-global.start] {
  for client_size in [0x34000,0x80000,client.end-client.start] {
   let g=global.start..global.start+global_size;let c=client.start..client.start+client_size;
   assert!(!overlap(&g,&c));
  }
 }
 assert!(0x7003f48000<=global.start); // both relocated completion rings
 assert_eq!(global.end,client.start);assert_eq!(client.end,G17P_CL_LOW_VA_END);
}
'''
with tempfile.TemporaryDirectory() as d:
 f=Path(d)/'test.rs';f.write_text(constants+functions+tests);exe=Path(d)/'test'
 subprocess.run(['rustc','--edition=2021','--test',str(f),'-o',str(exe)],check=True)
 subprocess.run([str(exe)],check=True)
print('PASS actual VA arena functions: captured old collision reproduced; complete canonical/private ranges disjoint')

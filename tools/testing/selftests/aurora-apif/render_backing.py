#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Exercise the production clear/repopulate methods with host-backed bytes."""
from pathlib import Path
import hashlib
import subprocess
import tempfile

temporary = tempfile.TemporaryDirectory(prefix='asahi-render-backing-')
root = Path(temporary.name)
tree = Path(__file__).resolve().parents[4] / 'drivers/gpu/drm/asahi/g17'
source = (tree / 'freelist.rs').read_text()

def method(name):
    start = source.index('    fn ' + name + '(')
    brace = source.index('{', start)
    end, depth = brace + 1, 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end].replace('super::fw::m5::pool', 'crate::pool')

test = r'''
#![allow(dead_code)]
use std::sync::atomic::{fence,Ordering};
type Result<T=()> = std::result::Result<T,i32>;
const EINVAL:i32=22; const EOVERFLOW:i32=75; const GFP_KERNEL:u32=0;
struct KVec<T>(Vec<T>);
impl<T> KVec<T> {
 fn with_capacity(n:usize,_flags:u32)->Result<Self>{Ok(Self(Vec::with_capacity(n)))}
 fn push(&mut self,x:T,_flags:u32)->Result {self.0.push(x);Ok(())}
}
impl<T> std::ops::Deref for KVec<T>{type Target=[T];fn deref(&self)->&[T]{&self.0}}
struct KernelObject{bytes:Vec<u8>,address:u64}
impl KernelObject{fn bytes_mut(&mut self)->&mut [u8]{&mut self.bytes}fn gpu_va(&self)->u64{self.address}}
enum PageLayout{M5(pool::PoolGeometry),Neo}
struct Pages{backing:KernelObject,layout:PageLayout}
impl Pages {
'''
test += method('populate_m5') + '\n' + method('reset_for_client') + '\n}\n'
test += f'#[path="{tree / "fw/m5/pool.rs"}"] mod pool;\n'
test += r'''
#[test] fn reused_storage_matches_fresh_tables_and_clears_all_old_data() {
 for groups in [1,7] {
  let g=pool::PoolGeometry::g17s(groups).unwrap();
  let tables=g.directory_bytes+g.freelist_bytes;
  let size=tables+g.blocks*g.block_bytes();
  let mut fresh=Pages{backing:KernelObject{bytes:vec![0;size],address:0x100000000},layout:PageLayout::M5(g)};
  fresh.populate_m5().unwrap();
  let mut old=Pages{backing:KernelObject{bytes:vec![0xa5;size],address:0x100000000},layout:PageLayout::M5(g)};
  old.reset_for_client().unwrap();
  assert_eq!(old.backing.bytes,fresh.backing.bytes);
  assert!(old.backing.bytes[tables..].iter().all(|b|*b==0));
  assert!(old.backing.bytes[..tables].iter().any(|b|*b!=0));
 }
}
'''
path = root / 'backing-tests.rs'
path.write_text(test)
subprocess.run(['rustc', '--edition=2021', '--test', str(path), '-o', str(root / 'backing-tests')], check=True)
run = subprocess.run([str(root / 'backing-tests')], capture_output=True, text=True)
(root / 'backing-test.log').write_text(run.stdout + run.stderr)
print('Production freelist.rs SHA256:', hashlib.sha256(source.encode()).hexdigest())
print(run.stdout, end='')
raise SystemExit(run.returncode)

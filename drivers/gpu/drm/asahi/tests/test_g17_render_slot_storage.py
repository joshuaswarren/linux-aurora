#!/usr/bin/env python3
"""Execute production owner-state swaps, including allocation failure and reuse."""
from pathlib import Path
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = (root / 'g17_manager.rs').read_text()


def item(marker):
    start = source.index(marker)
    end = source.index('{', start) + 1
    depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


swap = item('fn swap_active_render_slot_state(')
pairs = re.findall(r'core::mem::swap\(&mut self\.(\w+), &mut state\.(\w+)\)', swap)
state = item('struct G17PRenderSlotState {')
fields = dict(re.findall(r'^    (\w+): (.+),$', state, re.M))
assert {field for _, field in pairs} == set(fields), 'every owner field must move'
assert 'KBox::init(try_init!(Self {' in item('impl G17PRenderSlotState {')
assert 'KVec<Option<KBox<G17PRenderSlotState>>>' in source


def field_type(field):
    return 'Option<Arc<Token>>' if fields[field].startswith('Option<') else fields[field]


state_fields = ','.join(f'{f}:{field_type(f)}' for f in fields)
manager_fields = ','.join(f'{m}:{field_type(f)}' for m, f in pairs)
defaults = ','.join(f'{m}:Default::default()' for m, _ in pairs)
set_fields = '\n'.join(
    f'm.{m}=Some(token.clone());' if fields[f].startswith('Option<') else
    f'm.{m}=true;' if fields[f] == 'bool' else f'm.{m}=u64::from(slot)+1;'
    for m, f in pairs if f != 'trace_capture'
)
check_fields = '\n'.join(
    f'assert_eq!(m.{m}.as_ref().unwrap().owner,slot);' if fields[f].startswith('Option<') else
    f'assert!(m.{m});' if fields[f] == 'bool' else f'assert_eq!(m.{m},u64::from(slot)+1);'
    for m, f in pairs if f != 'trace_capture'
)
code = r'''
#![allow(dead_code)]
use std::sync::{Arc,atomic::{AtomicUsize,AtomicBool,Ordering}};
type KBox<T>=Box<T>;type Result<T=()>=std::result::Result<T,i32>;
const EINVAL:i32=22;const EIO:i32=5;
static ALLOCATIONS:AtomicUsize=AtomicUsize::new(0);
static FAIL:AtomicBool=AtomicBool::new(false);
struct Token{owner:u8,drops:Arc<AtomicUsize>}
impl Drop for Token{fn drop(&mut self){self.drops.fetch_add(1,Ordering::SeqCst);}}
mod g17_resources{pub struct G17PRenderQueuePair;impl G17PRenderQueuePair{pub const SLOT_COUNT:u8=64;}}
''' + f'''
#[derive(Default)]struct G17PRenderSlotState{{{state_fields}}}
impl G17PRenderSlotState{{fn new()->Result<KBox<Self>>{{
 if FAIL.swap(false,Ordering::SeqCst){{return Err(12)}}
 ALLOCATIONS.fetch_add(1,Ordering::SeqCst);Ok(Box::default())
}}}}
struct Manager{{{manager_fields},active_render_slot:u8,
 parked_render_slots:Vec<Option<KBox<G17PRenderSlotState>>>,
 render_slot_scratch:Option<KBox<G17PRenderSlotState>>}}
impl Manager{{
 fn new()->Self{{Self{{{defaults},active_render_slot:0,
 parked_render_slots:(0..64).map(|_|None).collect(),
 render_slot_scratch:Some(G17PRenderSlotState::new().unwrap())}}}}
 {swap}
 {item('fn take_active_render_slot_state(')}
 {item('fn install_active_render_slot_state(')}
 {item('pub(crate) fn select_render_slot(')}
}}
#[test]fn owner_switches_preserve_every_reference_without_steady_allocations(){{
 ALLOCATIONS.store(0,Ordering::SeqCst);
 let drops=Arc::new(AtomicUsize::new(0));let mut m=Manager::new();
 for slot in 0..64u8{{
  m.select_render_slot(slot).unwrap();let token=Arc::new(Token{{owner:slot,drops:drops.clone()}});
  {set_fields}
 }}
 let allocations=ALLOCATIONS.load(Ordering::SeqCst);assert_eq!(allocations,64);
 for _ in 0..100{{for slot in (0..64u8).rev(){{
  m.select_render_slot(slot).unwrap();assert!(m.parked_render_slots[slot as usize].is_none());
  {check_fields}
 }}}}
 assert_eq!(ALLOCATIONS.load(Ordering::SeqCst),allocations);
 assert_eq!(drops.load(Ordering::SeqCst),0);drop(m);assert_eq!(drops.load(Ordering::SeqCst),64);
}}
#[test]fn failed_first_selection_keeps_the_live_owner_intact(){{
 let drops=Arc::new(AtomicUsize::new(0));let mut m=Manager::new();let slot=0u8;
 let token=Arc::new(Token{{owner:slot,drops:drops.clone()}});{set_fields}drop(token);
 FAIL.store(true,Ordering::SeqCst);assert_eq!(m.select_render_slot(63),Err(12));
 assert_eq!(m.active_render_slot,0);assert!(m.parked_render_slots[0].is_none());
 {check_fields}
 assert_eq!(drops.load(Ordering::SeqCst),0);
 assert_eq!(m.select_render_slot(64),Err(EINVAL));
 m.select_render_slot(63).unwrap();m.select_render_slot(0).unwrap();{check_fields}
}}
'''
with tempfile.TemporaryDirectory(prefix='g17-slot-storage-') as tmp:
    src = Path(tmp) / 'test.rs'
    exe = Path(tmp) / 'test'
    src.write_text(code)
    subprocess.run(['rustc', '--edition=2021', '--test', str(src), '-o', str(exe)], check=True)
    subprocess.run([str(exe), '--test-threads=1'], check=True)

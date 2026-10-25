// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Typed publication identity of a singleton compute submission.
//! This metadata never owns its packet, aggregate fence or scheduler job.

use super::{context::Context, fence::{self, CompletionFence}, fw::kick::KickTimestamp};
use core::sync::atomic::{AtomicBool, Ordering};
use kernel::{bindings, dma_fence::{Fence, RawDmaFence, UserFence}, new_mutex,
    prelude::*, sync::{Arc, Mutex}};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Token {
    pub(crate) owner: u64,
    pub(crate) sequence: u64,
    pub(crate) context: u32,
    pub(crate) qid: u8,
    pub(crate) kick: KickTimestamp,
    pub(crate) epoch: u64,
    pub(crate) ordinal: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Snapshot {
    Pending,
    Published(Token),
    Complete,
}

struct State {
    token: Option<Token>,
    complete: bool,
    error: Option<Error>,
}

#[pin_data]
pub(crate) struct Source {
    context: Arc<Context>,
    ready: UserFence<CompletionFence>,
    signalled: AtomicBool,
    #[pin]
    state: Mutex<State>,
}

impl Source {
    pub(crate) fn check(&self) -> Result {
        if let Some(error) = self.state.lock().error { return Err(error); }
        if self.context.status().get() != 0 { return Err(EIO); }
        Ok(())
    }

    /// A GPU retirement witness is sufficient for a source with no CPU output.
    /// This does not signal any fence while the device mutex is held.
    pub(crate) fn note_successful_retirement(&self) {
        let mut state = self.state.lock();
        if state.error.is_none() { state.complete = true; state.token = None; }
    }

    pub(crate) fn new(ready: UserFence<CompletionFence>, context: Arc<Context>) -> Result<Arc<Self>> {
        Arc::pin_init(pin_init!(Self {
            context, ready, signalled: AtomicBool::new(false),
            state <- new_mutex!(State { token: None, complete: false, error: None },
                                "G17 compute source"),
        }), GFP_KERNEL)
    }

    pub(crate) fn same_vm(&self, consumer: &Context) -> bool {
        Arc::ptr_eq(self.context.status(), consumer.status())
    }

    pub(crate) fn ready_fence(&self) -> Fence {
        Fence::from_fence(&self.ready)
    }

    /// Called under the device lock only after the full publication succeeded.
    /// Its ready fence is signalled separately outside that lock.
    pub(crate) fn published(&self, token: Token) -> Result {
        if token.context != self.context.id() { return Err(EINVAL); }
        let mut state = self.state.lock();
        if let Some(error) = state.error { return Err(error); }
        if state.complete { return Err(EINVAL); }
        state.token = Some(token);
        Ok(())
    }

    /// A previous epoch's token is never a usable dependency after recovery.
    pub(crate) fn snapshot(&self, epoch: u64) -> Result<Snapshot> {
        let state = self.state.lock();
        if let Some(error) = state.error { return Err(error); }
        if self.context.status().get() != 0 { return Err(EIO); }
        if state.complete { return Ok(Snapshot::Complete); }
        match state.token {
            Some(token) if token.epoch == epoch => Ok(Snapshot::Published(token)),
            _ => Ok(Snapshot::Pending),
        }
    }

    /// All methods which can signal this fence run outside the device lock.
    pub(crate) fn signal_ready(&self) {
        if !self.signalled.swap(true, Ordering::AcqRel) {
            if let Some(error) = self.state.lock().error { self.ready.set_error(error); }
            self.ready.signal();
        }
    }

    pub(crate) fn complete(&self, result: Result) {
        {
            let mut state = self.state.lock();
            if let Err(error) = result { state.error.get_or_insert(error); }
            state.complete = result.is_ok() && state.error.is_none();
            state.token = None;
        }
        self.signal_ready();
    }
}

pub(crate) struct Dependency {
    pub(crate) source: Arc<Source>,
}

impl Dependency {
    pub(crate) fn new(source: Arc<Source>) -> Self { Self { source } }
    pub(crate) fn check(&self) -> Result { self.source.check() }
}

struct ChainReference(*mut bindings::dma_fence);

impl Drop for ChainReference {
    fn drop(&mut self) {
        // SAFETY: This is our reference; dma_fence_put accepts null as well.
        unsafe { bindings::dma_fence_put(self.0) };
    }
}

/// Expand supported nested containers without discarding a timeline prefix.
/// The budgets bound retained nodes/references, not internal kernel GC work.
#[inline(never)]
pub(crate) fn collect(fence: &Fence, consumer: &Context) -> Result<Option<KVec<Arc<Source>>>> {
    const SOURCES: usize = 31;
    const NODES: usize = 128;
    const STACK: usize = 64;
    let mut sources: KVec<Arc<Source>> = KVec::new();
    let mut stack: [Option<ChainReference>; STACK] = core::array::from_fn(|_| None);
    let mut seen = [0usize; NODES];
    let mut count = 0;
    let mut top = 1;
    // SAFETY: The input Fence owns its object; take an independent DFS reference.
    unsafe { bindings::dma_fence_get(fence.raw()) };
    stack[0] = Some(ChainReference(fence.raw()));
    while top != 0 {
        top -= 1;
        let mut current = stack[top].take().ok_or(EIO)?;
        // SAFETY: current owns this live fence throughout the lookup.
        let status = unsafe { bindings::dma_fence_get_status(current.0) };
        if status < 0 { return Ok(None); }
        if status > 0 { continue; }
        let address = current.0 as usize;
        if seen[..count].contains(&address) { continue; }
        if count == NODES { return Ok(None); }
        seen[count] = address;
        count += 1;
        // SAFETY: Ops is immutable in an initialized, retained dma_fence.
        let ops = unsafe { (*current.0).ops };
        if ops == core::ptr::addr_of!(bindings::dma_fence_chain_ops) {
            // SAFETY: Exact kernel chain vtable identifies the layout. The
            // retained chain owns its immutable contained fence until get.
            let child = unsafe {
                let raw = (*(current.0 as *const bindings::dma_fence_chain)).fence;
                if raw.is_null() { return Ok(None); }
                bindings::dma_fence_get(raw);
                ChainReference(raw)
            };
            let old = core::mem::replace(&mut current.0, core::ptr::null_mut());
            // SAFETY: chain_walk consumes old and returns an owned previous
            // node. Its internal GC may skip already-signalled predecessors.
            let previous = ChainReference(unsafe { bindings::dma_fence_chain_walk(old) });
            if !previous.0.is_null() {
                if top == STACK { return Ok(None); }
                stack[top] = Some(previous); top += 1;
            }
            if top == STACK { return Ok(None); }
            stack[top] = Some(child); top += 1;
        } else if ops == core::ptr::addr_of!(bindings::dma_fence_array_ops) {
            // SAFETY: The array stays retained until every selected child has
            // gained its own reference. The helper proves conjunction mode.
            let Some((entries, length)) = (unsafe { super::source_array::children(current.0) }) else {
                return Ok(None);
            };
            if length > STACK - top { return Ok(None); }
            for index in (0..length).rev() {
                // SAFETY: The immutable array owns each bounded entry.
                let raw = unsafe { *entries.add(index) };
                if raw.is_null() { return Ok(None); }
                unsafe { bindings::dma_fence_get(raw) };
                stack[top] = Some(ChainReference(raw)); top += 1;
            }
        } else {
            // SAFETY: The current DFS reference retains the typed object.
            let Some(source) = (unsafe { fence::exported_source_raw(current.0) }) else {
                return Ok(None);
            };
            if !source.same_vm(consumer) { return Ok(None); }
            if !sources.iter().any(|known| Arc::ptr_eq(known, &source)) {
                if sources.len() == SOURCES { return Ok(None); }
                if sources.is_empty() { sources = KVec::with_capacity(SOURCES, GFP_KERNEL)?; }
                sources.push_within_capacity(source).map_err(|_| ENOMEM)?;
            }
        }
    }
    Ok(Some(sources))
}

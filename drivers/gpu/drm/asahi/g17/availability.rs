// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Resource-specific scheduler waits. A caller samples before a failed try,
//! then registers against that sample after rolling back unpublished claims.
//! Each resource has its own domain, so rollback of another resource cannot
//! wake a waiter. Successful acquisition never allocates a fence.

use super::fence::{self, CompletionFence};
use kernel::{
    c_str,
    dma_fence::{Fence, FenceContexts, RawDmaFence, UserFence},
    new_mutex,
    prelude::*,
    sync::{Arc, Mutex},
};

struct Entry {
    id: u64,
    fence: UserFence<CompletionFence>,
    next: Option<KBox<Entry>>,
}

struct State {
    generation: u64,
    next: u64,
    waiters: Option<KBox<Entry>>,
}

#[pin_data]
pub(crate) struct Availability {
    #[pin]
    state: Mutex<State>,
    fences: FenceContexts,
}

impl Availability {
    pub(crate) fn new() -> Result<Arc<Self>> {
        Arc::pin_init(try_pin_init!(Self {
            state <- new_mutex!(State {
                generation: 0,
                next: 0,
                waiters: None,
            }, "G17 resource availability"),
            fences: FenceContexts::new(1, c_str!("asahi_resource"), kernel::static_lock_class!())?,
        }), GFP_KERNEL)
    }

    /// Sample before checking capacity under the resource's own synchronization.
    pub(crate) fn observe(&self) -> u64 { self.state.lock().generation }

    pub(crate) fn has_waiters(&self) -> bool { self.state.lock().waiters.is_some() }

    /// None means capacity changed after the sample: retry instead of sleeping.
    /// Called with no resource lock held. Recheck after allocation closes both
    /// the failed-check/registration and fence-allocation/release races.
    pub(crate) fn wait(self: &Arc<Self>, observed: u64) -> Result<Option<Wait>> {
        if self.observe() != observed { return Ok(None); }
        let fence = fence::independent(&self.fences)?;
        let mut entry = KBox::new(Entry { id: 0, fence: fence.clone(), next: None }, GFP_KERNEL)?;
        let mut state = self.state.lock();
        if state.generation != observed { return Ok(None); }
        let id = state.next.checked_add(1).ok_or(EOVERFLOW)?;
        entry.id = id;
        entry.next = state.waiters.take();
        state.waiters = Some(entry);
        state.next = id;
        Ok(Some(Wait { domain: self.clone(), id, fence, stop: None }))
    }

    /// Call only after dropping the resource lock. DMA-fence callbacks run
    /// outside the domain lock too, including during cancellation.
    pub(crate) fn notify(&self) { self.notify_inner(None); }

    /// Notify everyone after restoring this caller's temporary resource claim,
    /// excluding exactly that notification from its own blocked-check sample.
    /// Any intervening notification remains observable and forces a retry.
    pub(crate) fn notify_own(&self, observed: &mut u64) {
        self.notify_inner(Some(observed));
    }

    fn notify_inner(&self, observed: Option<&mut u64>) {
        let mut waiters = {
            let mut state = self.state.lock();
            let previous = state.generation;
            state.generation = previous.wrapping_add(1);
            if let Some(observed) = observed {
                if *observed == previous { *observed = state.generation; }
            }
            state.waiters.take()
        };
        while let Some(mut entry) = waiters {
            waiters = entry.next.take();
            entry.fence.signal();
        }
    }

    fn cancel(&self, id: u64) {
        let entry = {
            let mut state = self.state.lock();
            let mut link = &mut state.waiters;
            loop {
                match link.as_ref() {
                    None => break None,
                    Some(entry) if entry.id == id => {
                        let mut entry = link.take().expect("matched availability entry");
                        *link = entry.next.take();
                        break Some(entry);
                    }
                    _ => link = &mut link.as_mut().expect("nonempty availability link").next,
                }
            }
        };
        if let Some(entry) = entry { entry.fence.signal(); }
    }
}

/// The packet owns this registration; the scheduler owns only a fence clone.
/// No domain entry owns a packet, backend, context or resource reservation.
pub(crate) struct Wait {
    domain: Arc<Availability>,
    id: u64,
    fence: UserFence<CompletionFence>,
    stop: Option<(Arc<Availability>, u64)>,
}
impl Wait {
    pub(crate) fn fence(&self) -> Fence { Fence::from_fence(&self.fence) }

    /// Also wake on device stop, sharing the resource fence. The caller samples
    /// stop before checking device health. No stop can be lost while a resource
    /// wait is being allocated/registered; a changed sample signals immediately.
    pub(crate) fn also_wake(&mut self, domain: &Arc<Availability>, observed: u64) -> Result {
        if self.stop.is_some() { return Err(EINVAL); }
        if domain.observe() != observed {
            self.fence.signal();
            return Ok(());
        }
        let mut entry = KBox::new(Entry {
            id: 0, fence: self.fence.clone(), next: None,
        }, GFP_KERNEL)?;
        let mut state = domain.state.lock();
        if state.generation != observed {
            drop(state);
            self.fence.signal();
            return Ok(());
        }
        let id = state.next.checked_add(1).ok_or(EOVERFLOW)?;
        entry.id = id;
        entry.next = state.waiters.take();
        state.waiters = Some(entry);
        state.next = id;
        self.stop = Some((domain.clone(), id));
        Ok(())
    }
}
impl Drop for Wait {
    fn drop(&mut self) {
        self.domain.cancel(self.id);
        if let Some((domain, id)) = self.stop.take() { domain.cancel(id); }
    }
}

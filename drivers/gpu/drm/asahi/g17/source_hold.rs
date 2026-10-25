// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Physical compute binding readers retained until consumer retirement.

use super::availability::Availability;
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::{prelude::*, sync::Arc};

pub(crate) struct SourceHold {
    readers: Arc<AtomicUsize>,
    available: Arc<Availability>,
    notify: bool,
}

impl SourceHold {
    /// The caller holds the device mutex and has validated the exact source.
    /// Cloning the retained references performs no allocation.
    pub(crate) fn new(readers: &Arc<AtomicUsize>, available: &Arc<Availability>) -> Result<Self> {
        readers.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| count.checked_add(1))
            .map_err(|_| EOVERFLOW)?;
        Ok(Self { readers: readers.clone(), available: available.clone(), notify: true })
    }

    /// Roll back under the same device critical section that acquired this
    /// unpublished hold. No other submitter could have observed its count.
    pub(crate) fn drop_quiet(mut self) {
        self.notify = false;
    }
}

impl Drop for SourceHold {
    fn drop(&mut self) {
        let last = self.readers.fetch_sub(1, Ordering::AcqRel) == 1;
        // Normal retirement calls this outside the device mutex: notifications
        // can run fence callbacks. Quiet rollback never runs those callbacks.
        if last && self.notify { self.available.notify(); }
    }
}

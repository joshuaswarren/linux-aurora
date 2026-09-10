// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Exclusive context 3 for the bounded M5 compute experiment.
//!
//! Native M5 compute has a private lower root and a distinct empty upper
//! root. Its firmware-global root is not copied into the application slot.
//! The boot session retains this owner until both processors stop, including
//! on failure. This is not a general userspace context allocator.

use super::*;

const CONTEXT: usize = 3;

pub(crate) struct M5ExecutionContext {
    inner: Arc<UatInner>,
    low: Vm,
    high: Vm,
    published: bool,
}

impl Uat {
    pub(crate) fn new_t8142_execution_context(&self) -> Result<M5ExecutionContext> {
        if self.cfg != UatConfig::T8142 {
            return Err(ENODEV);
        }
        let range = IOVA_USER_BASE..uat_geometry(self.cfg)?.user_va_top();
        let low = self.new_vm(0x8142_0003, range.clone())?;
        // No mapping API for this VM escapes the context owner.
        let high = self.new_vm(0x8142_1003, range)?;
        Ok(M5ExecutionContext { inner: self.inner.clone(), low, high, published: false })
    }
}

impl M5ExecutionContext {
    pub(crate) fn vm(&self) -> &Vm { &self.low }

    pub(crate) fn roots(&self) -> [u64; 2] {
        let tag = (CONTEXT as u64) << TTBR_ASID_SHIFT | TTBR_VALID;
        [self.low.ttb() | tag, self.high.ttb() | tag]
    }

    /// Publish only after all command-owned mappings have been prepared.
    /// An occupied slot is never replaced, even by a second M5 probe owner.
    pub(crate) fn publish(&mut self) -> Result {
        if self.published { return Err(EBUSY); }
        let roots = self.roots();
        {
            let inner = self.inner.lock();
            let slot = &inner.ttbs()[CONTEXT];
            if slot.ttb0.load(Ordering::Acquire) != 0
                || slot.ttb1.load(Ordering::Acquire) != 0 {
                return Err(EBUSY);
            }
            slot.ttb0.store(roots[0], Ordering::Release);
            slot.ttb1.store(roots[1], Ordering::Release);
            self.published = true;
        }
        fence(Ordering::SeqCst);
        mem::tlbi_asid(CONTEXT as u8);
        mem::sync();
        Ok(())
    }

    pub(crate) fn is_current(&self) -> bool {
        if !self.published { return false; }
        let roots = self.roots();
        let inner = self.inner.lock();
        let slot = &inner.ttbs()[CONTEXT];
        slot.ttb0.load(Ordering::Acquire) == roots[0]
            && slot.ttb1.load(Ordering::Acquire) == roots[1]
    }
}

impl Drop for M5ExecutionContext {
    fn drop(&mut self) {
        if !self.published { return; }
        let roots = self.roots();
        {
            let inner = self.inner.lock();
            let slot = &inner.ttbs()[CONTEXT];
            // Only our exact pair may be retracted. VM owners remain alive
            // through root invalidation and the final ASID synchronization.
            if slot.ttb0.load(Ordering::Acquire) == roots[0]
                && slot.ttb1.load(Ordering::Acquire) == roots[1] {
                slot.ttb0.store(0, Ordering::Release);
                slot.ttb1.store(0, Ordering::Release);
            }
        }
        fence(Ordering::SeqCst);
        mem::tlbi_asid(CONTEXT as u8);
        mem::sync();
    }
}

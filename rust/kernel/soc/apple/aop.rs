// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Common code for AOP endpoint drivers

use kernel::{
    device::Device,
    prelude::*,
    sync::{
        atomic::{Atomic, Relaxed},
        Arc, //
    },
    types::ForeignOwnable,
};

/// Representation of an "EPIC" service.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct EPICService {
    /// Channel id
    pub channel: u32,
    /// RTKit endpoint
    pub endpoint: u8,
}

/// Listener for the "HID" events sent by aop.
///
/// The AOP driver keeps the listener in an [`Arc`] shared with the child driver that registered
/// it and invokes it from its own RTKit receive worker, so listeners have to be `Send + Sync`.
pub trait FakehidListener: Send + Sync {
    /// Process the event.
    fn process_fakehid_report(&self, data: &[u8]) -> Result<()>;
}

/// Listener for any other EPIC report subtype on a service (e.g. the T8140
/// low-power microphone producer reports, subtype 0x20).
pub trait ReportListener {
    /// Process the report payload (everything after the EPIC headers).
    fn process_report(&self, subtype: u16, data: &[u8]) -> Result<()>;
}

/// T8140: the low-power microphone source ring.  The firmware accepts one
/// binding for its lifetime (a second bind on the same boot answers
/// 0x2000000000000009), so the AOP driver owns the allocation and hands the
/// same ring to every audio driver probe.
pub struct SourceRing {
    /// The ring, mapped through the audio child's IOMMU stream.
    pub buf: Coherent<[u8]>,
    /// IOVA the firmware produces into.
    pub iova: u64,
}

impl SourceRing {
    /// Size of the ring in bytes.
    pub fn size(&self) -> usize {
        self.buf.size()
    }
}

/// AOP communications manager.
pub trait AOP: Send + Sync {
    /// Calls a method on a specified service
    fn epic_call(&self, svc: &EPICService, subtype: u16, msg_bytes: &[u8]) -> Result<u32>;
    /// Just like epic_call, but also returns a value
    fn epic_call_ret(
        &self,
        svc: &EPICService,
        subtype: u16,
        msg_bytes: &[u8],
        ret_len: usize,
    ) -> Result<(u32, KVec<u8>)>;

    /// Adds the listener for the specified service. A service takes one listener at a time;
    /// a second registration fails with `EBUSY`.
    fn add_fakehid_listener(
        &self,
        svc: EPICService,
        listener: Arc<dyn FakehidListener>,
    ) -> Result<()>;
    /// Removes the listener for the specified service. Returns once no callback into the
    /// listener is running any more.
    fn remove_fakehid_listener(&self, svc: &EPICService) -> bool;
    /// Adds a report listener for one subtype on the specified service
    fn add_report_listener(
        &self,
        svc: EPICService,
        subtype: u16,
        listener: Arc<dyn ReportListener>,
    ) -> Result<()>;
    /// Remove the report listener for the specified service and subtype
    fn remove_report_listener(&self, svc: &EPICService, subtype: u16) -> bool;
    /// T8140: the low-power microphone source ring, `size` bytes mapped
    /// through `dev` (the audio child).  Allocated and bound to the firmware
    /// over the setup port on first use, then shared for the AOP's lifetime.
    /// Returns ENODEV on other generations.
    fn source_ring(&self, dev: &Device<Bound>, size: usize) -> Result<Arc<SourceRing>>;
    /// Internal method to detach the device.
    fn remove(&self);
}

impl dyn AOP {
    /// Returns the AOP core that registered the service device `child`.
    ///
    /// The core registers its service devices from a workqueue while it is still probing, and the
    /// driver core publishes a driver's data only once its probe has returned, so a child can be
    /// probed before the core's data exists. This returns `EPROBE_DEFER` in that case; the child's
    /// probe is retried once the core has bound.
    ///
    /// # Safety
    ///
    /// `child` must be a platform device that the AOP core driver registered for one of its
    /// services, and the caller must be probing it or be bound to it. The core unregisters its
    /// children before it releases its driver data, so that data is valid for the lookup.
    pub unsafe fn from_child(child: &Device) -> Result<Arc<dyn AOP>> {
        let parent = child.parent().ok_or(ENODEV)?;
        let ptr = parent.get_drvdata::<c_void>();
        if ptr.is_null() {
            return Err(EPROBE_DEFER);
        }
        // SAFETY: By this function's contract `ptr` is the AOP core's driver data: a pinned box
        // holding the core's driver type, which is `#[repr(transparent)]` over an `Arc<dyn AOP>`,
        // and it stays valid while `child` is registered.
        let aop = unsafe { Pin::<KBox<Arc<dyn AOP>>>::borrow(ptr) };
        Ok((*aop).clone())
    }
}

/// A child driver's fake-HID subscription on an AOP service.
///
/// The AOP core calls the listener from its receive worker for as long as the subscription
/// exists, so the child has to end it before it releases anything the listener uses. Call
/// [`FakehidRegistration::unregister`] from the driver's `unbind`, while those resources are
/// still alive; dropping the registration unregisters as well, as a fallback for a failed probe.
pub struct FakehidRegistration {
    aop: Arc<dyn AOP>,
    service: EPICService,
    registered: Atomic<bool>,
}

impl FakehidRegistration {
    /// Subscribes `listener` to the fake-HID reports of `service`.
    pub fn new(
        aop: Arc<dyn AOP>,
        service: EPICService,
        listener: Arc<dyn FakehidListener>,
    ) -> Result<Self> {
        aop.add_fakehid_listener(service, listener)?;
        Ok(Self {
            aop,
            service,
            registered: Atomic::new(true),
        })
    }

    /// Ends the subscription. Returns once no callback into the listener is running any more;
    /// repeated calls do nothing.
    pub fn unregister(&self) {
        if self.registered.xchg(false, Relaxed) {
            self.aop.remove_fakehid_listener(&self.service);
        }
    }
}

impl Drop for FakehidRegistration {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// Converts a text representation of a FourCC to u32
pub const fn from_fourcc(b: &[u8]) -> u32 {
    b[3] as u32 | (b[2] as u32) << 8 | (b[1] as u32) << 16 | (b[0] as u32) << 24
}

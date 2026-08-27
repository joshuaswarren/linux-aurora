// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Support for Apple ASC Mailbox.
//!
//! C header: [`include/linux/soc/apple/mailbox.h`](../../../../include/linux/gpio/driver.h)

use crate::{
    bindings,
    device,
    error::{
        from_err_ptr,
        to_result, //
    },
    prelude::*,
    str::CStrExt,
    types::{
        ForeignOwnable,
        ScopeGuard, //
    }, //
};

use core::marker::PhantomData;
use core::mem::MaybeUninit;

/// 96-bit message. What it means is up to the upper layer
pub type Message = bindings::apple_mbox_msg;

/// Mailbox receive callback
pub trait MailCallback {
    /// Callback context
    type Data: ForeignOwnable + Send + Sync;

    /// The actual callback. Called in an interrupt context.
    fn recv_message(data: <Self::Data as ForeignOwnable>::Borrowed<'_>, msg: Message);
}

/// Wrapper over `struct apple_mbox *`
#[repr(transparent)]
pub struct Mailbox<T: MailCallback> {
    mbox: *mut bindings::apple_mbox,
    _p: PhantomData<T>,
}

extern "C" fn mailbox_rx_callback<T: MailCallback>(
    _mbox: *mut bindings::apple_mbox,
    msg: Message,
    cookie: *mut crate::ffi::c_void,
) {
    // SAFETY: cookie came from a call to `into_foreign`
    T::recv_message(unsafe { T::Data::borrow(cookie.cast()) }, msg);
}

impl<T: MailCallback> Mailbox<T> {
    /// Creates a mailbox for the specified name.
    pub fn new_byname(
        dev: &device::Device,
        mbox_name: &'static CStr,
        data: T::Data,
    ) -> Result<Mailbox<T>> {
        let ptr: *mut crate::ffi::c_void = data.into_foreign().cast();
        let guard = ScopeGuard::new(|| {
            // SAFETY: `ptr` came from a previous call to `into_foreign`.
            unsafe { T::Data::from_foreign(ptr.cast()) };
        });
        // SAFETY: Just calling the c function, all values are valid.
        let mbox = unsafe {
            from_err_ptr(bindings::apple_mbox_get_byname(
                dev.as_raw(),
                mbox_name.as_char_ptr(),
            ))?
        };
        // SAFETY: mbox is a valid pointer
        unsafe {
            (*mbox).cookie = ptr;
            (*mbox).rx = Some(mailbox_rx_callback::<T>);
            if let Err(err) = to_result(bindings::apple_mbox_start(mbox)) {
                (*mbox).rx = None;
                (*mbox).cookie = core::ptr::null_mut();
                return Err(err);
            }
        }
        guard.dismiss();
        Ok(Mailbox {
            mbox,
            _p: PhantomData,
        })
    }
    /// Sends the specified message
    pub fn send(&self, msg: Message, atomic: bool) -> Result<()> {
        // SAFETY: Calling the c function, `mbox` is a valid pointer
        to_result(unsafe { bindings::apple_mbox_send(self.mbox, msg, atomic) })
    }
}

impl<T: MailCallback> Drop for Mailbox<T> {
    fn drop(&mut self) {
        // SAFETY: mbox is a valid pointer
        unsafe { bindings::apple_mbox_stop(self.mbox) };
        // SAFETY: RX is synchronously disabled above, and `cookie` came from
        // `into_foreign`. Clear the provider-visible callback before freeing
        // its context so provider removal can never observe a dangling owner.
        unsafe {
            let cookie = (*self.mbox).cookie;
            (*self.mbox).rx = None;
            (*self.mbox).cookie = core::ptr::null_mut();
            T::Data::from_foreign(cookie.cast());
        }
    }
}

unsafe impl<T> Sync for Mailbox<T>
where
    T: MailCallback,
    T::Data: Sync,
{
}

unsafe impl<T> Send for Mailbox<T>
where
    T: MailCallback,
    T::Data: Send,
{
}

/// Firmware role owned by a T8140 ASCWrap v6 mailbox provider.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AscwrapV6Role {
    /// Primary GPU firmware role.
    Gfx,
    /// Secondary GPU firmware role.
    Gfx1,
}

/// Provider-owned ASCWrap v6 lifecycle facts.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AscwrapV6Lifecycle {
    /// Firmware role owned by this provider.
    pub role: AscwrapV6Role,
    /// CPU physical base and size of the provider-owned wrapper window.
    pub wrapper: (u64, u64),
    /// CPU physical base and size of the provider-owned IOP VBAR window.
    pub iop_vbar: (u64, u64),
    /// Non-zero until a complete provider-owned stop/reset path exists.
    pub missing: u32,
}

/// Non-owning handle used to query an ASCWrap v6 mailbox provider.
///
/// The provider, not the consumer, maps the wrapper and IOP VBAR resources.
/// This handle deliberately exposes no MMIO pointer and no one-way CPU-start
/// primitive.
pub struct AscwrapV6Provider {
    mbox: *mut bindings::apple_mbox,
}

impl AscwrapV6Provider {
    /// Resolve one provider from a consumer's `mboxes` list.
    pub fn get(dev: &device::Device, index: usize) -> Result<Self> {
        // SAFETY: `dev` is a live device and the returned provider pointer is
        // tied to the automatic device link created by apple_mbox_get().
        let mbox =
            unsafe { from_err_ptr(bindings::apple_mbox_get(dev.as_raw(), index.try_into()?)) }?;
        Ok(Self { mbox })
    }

    /// Return the exact provider-owned resources and lifecycle gates.
    pub fn lifecycle(&self) -> Result<AscwrapV6Lifecycle> {
        let mut raw = MaybeUninit::<bindings::apple_mbox_ascwrap_v6_lifecycle>::uninit();
        // SAFETY: `self.mbox` is valid and `raw` points to writable storage.
        to_result(unsafe {
            bindings::apple_mbox_ascwrap_v6_get_lifecycle(self.mbox, raw.as_mut_ptr())
        })?;
        // SAFETY: the successful C call initialized every field.
        let raw = unsafe { raw.assume_init() };
        let role = match raw.role as u32 {
            // Values are fixed by enum apple_mbox_ascwrap_v6_role.
            1 => AscwrapV6Role::Gfx,
            2 => AscwrapV6Role::Gfx1,
            _ => return Err(EINVAL),
        };

        Ok(AscwrapV6Lifecycle {
            role,
            wrapper: (raw.wrapper_start, raw.wrapper_size),
            iop_vbar: (raw.iop_vbar_start, raw.iop_vbar_size),
            missing: raw.missing,
        })
    }

    /// Require a lifecycle that can be unwound after every partial failure.
    ///
    /// This currently returns `EOPNOTSUPP` before any CPU or IOP VBAR write,
    /// because the grounded provider-owned stop/reset implementation is not
    /// complete.
    pub fn require_safe_cpu_lifecycle(&self) -> Result {
        // SAFETY: this is a capability check on a valid provider pointer.  It
        // performs no MMIO writes while any lifecycle gate is missing.
        to_result(unsafe { bindings::apple_mbox_ascwrap_v6_require_safe_cpu_lifecycle(self.mbox) })
    }
}

// SAFETY: provider state is internally synchronized by the C mailbox driver;
// this wrapper exposes read-only queries and a fail-closed capability check.
unsafe impl Send for AscwrapV6Provider {}
// SAFETY: as above.
unsafe impl Sync for AscwrapV6Provider {}

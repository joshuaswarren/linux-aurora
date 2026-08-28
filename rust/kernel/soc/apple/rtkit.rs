// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Support for Apple RTKit coprocessors.
//!
//! C header: [`include/linux/soc/apple/rtkit.h`](../../../../include/linux/gpio/driver.h)

use crate::{
    alloc::flags::*,
    bindings,
    device,
    error::{
        from_err_ptr,
        from_result,
        to_result, //
    },
    iosys_map::IoSysMapRef,
    prelude::*,
    str::CStrExt,
    types::{
        ForeignOwnable,
        ScopeGuard, //
    }, //
};

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ptr;
use macros::vtable;

/// Trait to represent allocatable buffers for the RTKit core.
///
/// Users must implement this trait for their own representation of those allocations.
pub trait Buffer {
    /// Returns the IOVA (virtual address) of the buffer from RTKit's point of view, or an error if
    /// unavailable.
    fn iova(&self) -> Result<usize>;

    /// Returns a mutable byte slice of the buffer contents, or an
    /// error if unavailable.
    fn buf(&mut self) -> Result<IoSysMapRef<'_, u8>>;
}

/// Callback operations for an RTKit client.
#[vtable]
pub trait Operations {
    /// Arbitrary user context type.
    type Data: ForeignOwnable + Send + Sync;

    /// Type representing an allocated buffer for RTKit.
    type Buffer: Buffer;

    /// Called when RTKit crashes.
    fn crashed(_data: <Self::Data as ForeignOwnable>::Borrowed<'_>, _crashlog: Option<&[u8]>) {}

    /// Called when a message was received on a non-system endpoint. Called in non-IRQ context.
    fn recv_message(
        _data: <Self::Data as ForeignOwnable>::Borrowed<'_>,
        _endpoint: u8,
        _message: u64,
    ) {
    }

    /// Called in IRQ context when a message was received on a non-system endpoint.
    ///
    /// Must return `true` if the message is handled, or `false` to process it in
    /// the handling thread.
    fn recv_message_early(
        _data: <Self::Data as ForeignOwnable>::Borrowed<'_>,
        _endpoint: u8,
        _message: u64,
    ) -> bool {
        false
    }

    /// Allocate a buffer for use by RTKit.
    fn shmem_alloc(
        _data: <Self::Data as ForeignOwnable>::Borrowed<'_>,
        _size: usize,
    ) -> Result<Self::Buffer> {
        Err(EINVAL)
    }

    /// Map an existing buffer used by RTKit at a device-specified virtual address.
    fn shmem_map(
        _data: <Self::Data as ForeignOwnable>::Borrowed<'_>,
        _iova: usize,
        _size: usize,
    ) -> Result<Self::Buffer> {
        Err(EINVAL)
    }
}

/// Represents `struct apple_rtkit *`.
///
/// # Invariants
///
/// The rtk pointer is valid.
/// The data pointer is a valid pointer from T::Data::into_foreign().
pub struct RtKit<T: Operations> {
    rtk: *mut bindings::apple_rtkit,
    data: *mut core::ffi::c_void,
    _p: PhantomData<T>,
}

/// Exact result from a discovery-only HELLO/EPMAP exchange.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct EpmapSnapshot {
    /// Unmodified endpoint-zero HELLO message received from firmware.
    pub raw_hello: u64,
    /// HELLO minimum protocol version.
    pub hello_min_version: u16,
    /// HELLO maximum protocol version.
    pub hello_max_version: u16,
    /// Raw 256-bit endpoint bitmap, little-endian by endpoint number.
    pub endpoints: [u64; 4],
}

impl EpmapSnapshot {
    /// Return whether firmware advertised an endpoint in its EPMAP.
    pub fn has_endpoint(&self, endpoint: u8) -> bool {
        let word = endpoint as usize / 64;
        let bit = endpoint as usize % 64;
        self.endpoints[word] & (1u64 << bit) != 0
    }
}

unsafe extern "C" fn crashed_callback<T: Operations>(
    cookie: *mut core::ffi::c_void,
    crashlog: *const core::ffi::c_void,
    crashlog_size: usize,
) {
    let crashlog = if !crashlog.is_null() && crashlog_size > 0 {
        // SAFETY: The crashlog is either missing or a byte buffer of the specified size
        Some(unsafe { core::slice::from_raw_parts(crashlog as *const u8, crashlog_size) })
    } else {
        None
    };
    // SAFETY: cookie is always a T::Data in this API
    T::crashed(unsafe { T::Data::borrow(cookie.cast()) }, crashlog);
}

unsafe extern "C" fn recv_message_callback<T: Operations>(
    cookie: *mut core::ffi::c_void,
    endpoint: u8,
    message: u64,
) {
    // SAFETY: cookie is always a T::Data in this API
    T::recv_message(unsafe { T::Data::borrow(cookie.cast()) }, endpoint, message);
}

unsafe extern "C" fn recv_message_early_callback<T: Operations>(
    cookie: *mut core::ffi::c_void,
    endpoint: u8,
    message: u64,
) -> bool {
    // SAFETY: cookie is always a T::Data in this API
    T::recv_message_early(unsafe { T::Data::borrow(cookie.cast()) }, endpoint, message)
}

unsafe extern "C" fn shmem_setup_callback<T: Operations>(
    cookie: *mut core::ffi::c_void,
    bfr: *mut bindings::apple_rtkit_shmem,
) -> core::ffi::c_int {
    // SAFETY: `bfr` is a valid buffer
    let bfr_mut = unsafe { &mut *bfr };

    from_result(|| {
        let mut buf = if bfr_mut.iova != 0 {
            bfr_mut.is_mapped = true;
            T::shmem_map(
                // SAFETY: `cookie` came from a previous call to `into_foreign`.
                unsafe { T::Data::borrow(cookie.cast()) },
                bfr_mut.iova as usize,
                bfr_mut.size,
            )?
        } else {
            bfr_mut.is_mapped = false;
            // SAFETY: `cookie` came from a previous call to `into_foreign`.
            T::shmem_alloc(unsafe { T::Data::borrow(cookie.cast()) }, bfr_mut.size)?
        };

        let iova = buf.iova()?;
        let iosys_map = buf.buf()?;

        if iosys_map.size() < bfr_mut.size {
            return Err(ENOMEM);
        }

        bfr_mut.iova = iova as u64;
        bfr_mut.buffer = iosys_map.as_mut_ptr() as *mut _;

        // Now box the returned buffer type and stash it in the private pointer of the
        // `apple_rtkit_shmem` struct for safekeeping.
        let boxed = KBox::new(buf, GFP_KERNEL)?;
        bfr_mut.private = KBox::into_raw(boxed) as *mut _;
        Ok(0)
    })
}

unsafe extern "C" fn shmem_destroy_callback<T: Operations>(
    _cookie: *mut core::ffi::c_void,
    bfr: *mut bindings::apple_rtkit_shmem,
) {
    // SAFETY: `bfr` is a valid buffer
    let bfr_mut = unsafe { &mut *bfr };
    if !bfr_mut.private.is_null() {
        // SAFETY: Per shmem_setup_callback, this has to be a pointer to a Buffer if it is set.
        unsafe {
            core::mem::drop(KBox::from_raw(bfr_mut.private as *mut T::Buffer));
        }
        bfr_mut.private = core::ptr::null_mut();
    }
}

impl<T: Operations> RtKit<T> {
    const VTABLE: bindings::apple_rtkit_ops = bindings::apple_rtkit_ops {
        crashed: Some(crashed_callback::<T>),
        recv_message: Some(recv_message_callback::<T>),
        recv_message_early: Some(recv_message_early_callback::<T>),
        shmem_setup: if T::HAS_SHMEM_ALLOC || T::HAS_SHMEM_MAP {
            Some(shmem_setup_callback::<T>)
        } else {
            None
        },
        shmem_destroy: if T::HAS_SHMEM_ALLOC || T::HAS_SHMEM_MAP {
            Some(shmem_destroy_callback::<T>)
        } else {
            None
        },
    };

    /// Creates a new RTKit client for a given device and optional mailbox name or index.
    pub fn new(
        dev: &device::Device,
        mbox_name: Option<&'static CStr>,
        mbox_idx: usize,
        data: T::Data,
    ) -> Result<Self> {
        Self::new_inner(dev, mbox_name, mbox_idx, data, false, false)
    }

    /// Creates a G17P RTKit client with firmware-owned crash-DVA handling.
    pub fn new_g17p(
        dev: &device::Device,
        mbox_name: Option<&'static CStr>,
        mbox_idx: usize,
        data: T::Data,
    ) -> Result<Self> {
        Self::new_inner(dev, mbox_name, mbox_idx, data, false, true)
    }

    /// Create a passive management client that answers only HELLO/EPMAP.
    ///
    /// This mode never starts crashlog or another endpoint and never performs
    /// an AP/IOP power-state transition.
    pub fn new_epmap_only(
        dev: &device::Device,
        mbox_name: Option<&'static CStr>,
        mbox_idx: usize,
        data: T::Data,
    ) -> Result<Self> {
        Self::new_inner(dev, mbox_name, mbox_idx, data, true, false)
    }

    fn new_inner(
        dev: &device::Device,
        mbox_name: Option<&'static CStr>,
        mbox_idx: usize,
        data: T::Data,
        epmap_only: bool,
        g17p: bool,
    ) -> Result<Self> {
        let ptr: *mut crate::ffi::c_void = data.into_foreign().cast();
        let guard = ScopeGuard::new(|| {
            // SAFETY: `ptr` came from a previous call to `into_foreign`.
            unsafe { T::Data::from_foreign(ptr.cast()) };
        });
        // SAFETY: `dev` is valid by its type invarants and otherwise his just
        //          calls the C init function.
        let raw_name = match mbox_name {
            Some(s) => s.as_char_ptr(),
            None => ptr::null(),
        };
        let raw_index = mbox_idx.try_into()?;
        let rtk = if epmap_only {
            // SAFETY: arguments follow the C API contract and `ptr` remains
            // owned by this wrapper until construction fails or Drop runs.
            unsafe {
                from_err_ptr(bindings::apple_rtkit_init_epmap_only(
                    dev.as_raw(),
                    ptr,
                    raw_name,
                    raw_index,
                    &Self::VTABLE,
                ))
            }
        } else if g17p {
            // SAFETY: arguments follow the C API contract. This constructor
            // changes only the crashlog handshake used by G17P firmware.
            unsafe {
                from_err_ptr(bindings::apple_rtkit_init_g17p(
                    dev.as_raw(),
                    ptr,
                    raw_name,
                    raw_index,
                    &Self::VTABLE,
                ))
            }
        } else {
            // SAFETY: same invariants as the discovery-only constructor.
            unsafe {
                from_err_ptr(bindings::apple_rtkit_init(
                    dev.as_raw(),
                    ptr,
                    raw_name,
                    raw_index,
                    &Self::VTABLE,
                ))
            }
        }?;

        guard.dismiss();
        // INVARIANT: `rtk` and `data` are valid here.
        Ok(Self {
            rtk,
            data: ptr,
            _p: PhantomData,
        })
    }

    /// Boots (wakes up) the RTKit coprocessor.
    pub fn wake(self: Pin<&mut Self>) -> Result {
        // SAFETY: `rtk` is valid per the type invariant.
        to_result(unsafe { bindings::apple_rtkit_wake(self.rtk) })
    }

    /// Waits for the RTKit coprocessor to finish booting.
    pub fn boot(self: Pin<&mut Self>) -> Result {
        // SAFETY: `rtk` is valid per the type invariant.
        to_result(unsafe { bindings::apple_rtkit_boot(self.rtk) })
    }

    /// Wait for a passive HELLO/EPMAP exchange and return its exact snapshot.
    pub fn discover_epmap(self: Pin<&mut Self>) -> Result<EpmapSnapshot> {
        // SAFETY: `rtk` is valid per the type invariant and was constructed in
        // discovery-only mode; the C API rejects any other mode.
        to_result(unsafe { bindings::apple_rtkit_discover_epmap(self.rtk) })?;

        let mut raw = MaybeUninit::<bindings::apple_rtkit_epmap_snapshot>::uninit();
        // SAFETY: a successful discovery initialized the C snapshot.
        to_result(unsafe { bindings::apple_rtkit_get_epmap_snapshot(self.rtk, raw.as_mut_ptr()) })?;
        // SAFETY: the successful getter initialized every field.
        let raw = unsafe { raw.assume_init() };

        Ok(EpmapSnapshot {
            raw_hello: raw.raw_hello,
            hello_min_version: raw.hello_min_version,
            hello_max_version: raw.hello_max_version,
            endpoints: raw.endpoints,
        })
    }

    /// Starts a non-system endpoint.
    pub fn start_endpoint(self: Pin<&mut Self>, endpoint: u8) -> Result {
        // SAFETY: `rtk` is valid per the type invariant.
        to_result(unsafe { bindings::apple_rtkit_start_ep(self.rtk, endpoint) })
    }

    /// Sends a message to a given endpoint.
    pub fn send_message(self: Pin<&mut Self>, endpoint: u8, message: u64) -> Result {
        // SAFETY: `rtk` is valid per the type invariant.
        to_result(unsafe {
            bindings::apple_rtkit_send_message(self.rtk, endpoint, message, ptr::null_mut(), false)
        })
    }

    /// Checks if an endpoint is present
    pub fn has_endpoint(self: Pin<&mut Self>, endpoint: u8) -> bool {
        unsafe { bindings::apple_rtkit_has_endpoint(self.rtk, endpoint) }
    }
}

// SAFETY: `RtKit` operations require a mutable reference
unsafe impl<T: Operations> Sync for RtKit<T> {}

// SAFETY: `RtKit` operations require a mutable reference
unsafe impl<T: Operations> Send for RtKit<T> {}

impl<T: Operations> Drop for RtKit<T> {
    fn drop(&mut self) {
        // SAFETY: The pointer is valid by the type invariant.
        unsafe { bindings::apple_rtkit_free(self.rtk) };

        // Free context data.
        //
        // SAFETY: This matches the call to `into_foreign` from `new` in the success case.
        unsafe { T::Data::from_foreign(self.data.cast()) };
    }
}

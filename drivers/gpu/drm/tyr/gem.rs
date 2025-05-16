// SPDX-License-Identifier: GPL-2.0 or MIT
//! GEM buffer object management for the Tyr driver.
//!
//! This module provides buffer object (BO) management functionality using
//! DRM's GEM subsystem with shmem backing.

use kernel::{
    drm::{
        gem,
        DeviceContext, //
    },
    prelude::*, //
};

use crate::driver::{
    TyrDrmDevice,
    TyrDrmDriver, //
};

/// Tyr's DriverObject type for GEM objects.
#[pin_data]
pub(crate) struct BoData {
    flags: u32,
}

#[vtable]
impl gem::DriverObject for TyrObject {
    type Driver = TyrDrmDriver;
    type Args = BoCreateArgs;

    fn new<Ctx: DeviceContext>(
        _dev: &TyrDrmDevice<Ctx>,
        _size: usize,
        args: BoCreateArgs,
    ) -> impl PinInit<Self, Error> {
        try_pin_init!(Self { flags: args.flags })
    }
}

// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Apple AOP ambient light sensor driver
//!
//! Copyright (C) The Asahi Linux Contributors

use kernel::{
    bindings, c_str,
    device::Core,
    firmware::Firmware,
    iio::common::aop_sensors::{AopSensorData, IIORegistration, MessageProcessor},
    module_platform_driver, of, platform,
    prelude::*,
    soc::apple::aop::{EPICService, AOP},
    sync::Arc,
    types::ForeignOwnable,
};

const EPIC_SUBTYPE_GET_AOP_PROPERTY: u16 = 0xa;
const EPIC_SUBTYPE_SET_ALS_PROPERTY: u16 = 0x4;
/// The HID report descriptor.  macOS asks for this before anything else
/// (hypervisor trace, 2026-09-20); Linux never has, and deliberately does not
/// -- see the note in probe().  Retained as the identifier for that call.
#[allow(dead_code)]
const EPIC_SUBTYPE_GET_HID_DESCRIPTOR: u16 = 0x1;
/* The EPIC sub-header's `subtype` at +0x06 is macOS's SpuPacketType: Linux's
 * EPICHeaderV4 and macOS's _AOPSubPacket are the same 0x18-byte structure, and
 * the AFK ring is the same transport (Linux's QE_MAGIC2 " POA" is the "AOP "
 * magic of iop_ringbuffer_enqueue_msg_vec).  So the SPU is reachable with the
 * calls we already have -- only the payload differs.  macOS's
 * _spuGetPropertyGated sends the bare property id as the whole inline payload
 * (mov w2,#0xa; mov w4,#0x4), where the AOP `als` service takes a
 * [u32 zero, u32 tag] envelope.  That 4-byte prefix is why the earlier 256-tag
 * sweep of endpoint 0x20 was silent. */
const LUX_OFFSET_CT720: usize = 0x1d;
const LUX_OFFSET_VD6286: usize = 0x28;
/* CT817's report is exactly 32 bytes and the lux float sits at +0x1c, read off
 * AppleSPUCT817::handleInterruptReport (0xfffffe0008e417a8):
 *
 *     cmp  x0, #0x20 ; b.ne drop      ; length must be exactly 32
 *     ldp  q0, q1, [x22] ; stp q0, q1, [sp]
 *     ldr  s8,  [sp, #0x1c]           ; lux
 *     ldp  s0, s1, [sp, #0x8]         ; rawChannel0, rawChannel1
 *
 * This is one byte below the CT720's 0x1d, which CT817 was borrowing on the
 * assumption that the same family shares a layout. */
const LUX_OFFSET_CT817: usize = 0x1c;
/// The CT817's two raw channels.  Nothing reads them: they are kept because
/// together with LUX_OFFSET_CT817 and CT817_REPORT_LEN they are the complete
/// report layout of the part, established from real light, and that is the
/// thing to check a suspect reading against.
#[allow(dead_code)]
const RAW_CH0_OFFSET_CT817: usize = 0x08;
#[allow(dead_code)]
const RAW_CH1_OFFSET_CT817: usize = 0x0c;
#[allow(dead_code)]
const CT817_REPORT_LEN: usize = 0x20;

fn get_lux_offset(aop: &dyn AOP, dev: &platform::Device, svc: &EPICService) -> Result<usize> {
    let name = get_aop_property(aop, svc, 0xf, 16)?.1;
    match name.as_slice() {
        b"Redbird\0" => Ok(LUX_OFFSET_VD6286),
        b"FireFish2\0" => Ok(LUX_OFFSET_CT720),
        // J700 (MacBook Neo): the AOP reports the part number itself rather
        // than a codename.  CT817 does NOT share the CT720's layout -- its lux
        // float is at +0x1c of a 32-byte report, one byte below the CT720's
        // 0x1d (see LUX_OFFSET_CT817).
        b"CT817\0" => Ok(LUX_OFFSET_CT817),
        _ => {
            dev_warn!(
                dev.as_ref(),
                "Unknown sensor type {:?}",
                core::str::from_utf8(&name)
            );
            Err(EIO)
        }
    }
}


fn enable_als(aop: &dyn AOP, dev: &platform::Device, svc: &EPICService) -> Result<()> {

    // The calibration is per-device factory data that the installer extracts
    // from a running macOS.  The J700's ADT sets `als-new-calibration-path = 1`
    // and carries no blob of its own, which suggests the AOP already holds its
    // calibration on this generation -- so a missing file is a warning and the
    // sensor is still enabled, rather than failing probe outright.  A machine
    // that does need the blob simply reports uncalibrated lux until it is
    // supplied, which is visible and recoverable.
    if let Ok(fw) = Firmware::request(c_str!("apple/aop-als-cal.bin"), dev.as_ref()) {
        let rc = set_als_property(aop, svc, 0xb, fw.data())?;
        dev_info!(dev.as_ref(), "als: CALIBRATION (0xb) {} bytes -> rc {:#x}\n", fw.data().len(), rc);
    } else {
        dev_warn!(
            dev.as_ref(),
            "apple/aop-als-cal.bin is absent; enabling the sensor without pushing a calibration (the AOP may hold its own)\n"
        );
    }
    // Verbosity is written the way macOS writes it.  MODE (0xd7) and 0xe4 used
    // to be written here as well, on the theory that one of them armed a part on
    // the `als-new-calibration-path` generation.  Neither does, and both are
    // actively harmful: each wedges the AOP outright (measured 2026-09-21 -- the
    // call never answers and every later call on the endpoint times out).  What
    // actually arms this part is the calibration the AOP driver pushes on
    // setup-port endpoint 0x21; see soc/apple/aop.rs.  u32::MAX means "leave
    // alone", and a rejected write is logged rather than failing the probe.
    let try_prop = |tag: u32, val: u32, name: &'static str| {
        if val == u32::MAX {
            return;
        }
        match set_als_property(aop, svc, tag, &val.to_le_bytes()) {
            Ok(rc) => dev_info!(dev.as_ref(), "als: {} ({:#x}) = {:#x} -> rc {:#x}\n", name, tag, val, rc),
            Err(e) => dev_info!(dev.as_ref(), "als: {} ({:#x}) = {:#x} -> error {:?}\n", name, tag, val, e),
        }
    };

    try_prop(0xe1, *module_parameters::set_verbosity.value(), "VERBOSITY");

    // macOS never writes the report interval on its own.  
    // AppleSPUHIDDriver::setPropertiesGated (0xfffffe00096cfb80..0xfffffe00096cfbac)
    // writes three rate properties in a fixed order every time:
    //
    //     SetProperty(0x16, batchInterval, 4)   ; BatchInterval
    //     SetProperty(0x5c, rawInterval,   4)   ; RawInterval
    //     SetProperty(0x00, reportInterval,4)   ; ReportInterval
    //
    // and it is the 0 -> non-zero transition of 0x00 that calls
    // startSensorWatchdog() (0xfffffe00096cfb5c) -- writing the same non-zero
    // value again does nothing.  We had only ever written 0x00, once, which the
    // AOP acks with rc 0 and then sends nothing.  Write zero first so the
    // transition is guaranteed, then the full triple in macOS's order.
    // CT817's own tables (0xfffffe000738e420) pair report 200000 us with batch
    // 500000 us at the slow rate.
    let ivl = *module_parameters::interval_us.value();
    let batch = *module_parameters::batch_interval_us.value();
    let raw = *module_parameters::raw_interval_us.value();

    match set_als_property(aop, svc, 0x16, &batch.to_le_bytes()) {
        Ok(rc) => dev_info!(dev.as_ref(), "als: BATCH_INTERVAL (0x16) = {} us -> rc {:#x}\n", batch, rc),
        Err(e) => dev_info!(dev.as_ref(), "als: BATCH_INTERVAL (0x16) = {} us -> error {:?}\n", batch, e),
    }
    match set_als_property(aop, svc, 0x5c, &raw.to_le_bytes()) {
        Ok(rc) => dev_info!(dev.as_ref(), "als: RAW_INTERVAL (0x5c) = {} us -> rc {:#x}\n", raw, rc),
        Err(e) => dev_info!(dev.as_ref(), "als: RAW_INTERVAL (0x5c) = {} us -> error {:?}\n", raw, e),
    }
    let rc = set_als_property(aop, svc, 0, &ivl.to_le_bytes())?;
    dev_info!(dev.as_ref(), "als: INTERVAL (0x0) = {} us -> rc {:#x}\n", ivl, rc);

    Ok(())
}

fn get_aop_property(
    aop: &dyn AOP,
    svc: &EPICService,
    tag: u32,
    data_len: usize,
) -> Result<(u32, KVec<u8>)> {
    let mut buf = KVec::new();
    buf.resize(8, 0, GFP_KERNEL)?;
    buf[4..8].copy_from_slice(&tag.to_le_bytes());
    aop.epic_call_ret(svc, EPIC_SUBTYPE_GET_AOP_PROPERTY, &buf, data_len)
}



fn set_als_property(aop: &dyn AOP, svc: &EPICService, tag: u32, data: &[u8]) -> Result<u32> {
    let mut buf = KVec::new();
    buf.resize(data.len() + 8, 0, GFP_KERNEL)?;
    buf[8..].copy_from_slice(data);
    buf[4..8].copy_from_slice(&tag.to_le_bytes());
    aop.epic_call(svc, EPIC_SUBTYPE_SET_ALS_PROPERTY, &buf)
}

fn f32_to_u32(f: u32) -> u32 {
    if f & 0x80000000 != 0 {
        return 0;
    }
    let exp = ((f & 0x7f800000) >> 23) as i32 - 127;
    if exp < 0 {
        return 0;
    }
    if exp == 128 && f & 0x7fffff != 0 {
        return 0;
    }
    let mant = f & 0x7fffff | 0x800000;
    if exp <= 23 {
        return mant >> (23 - exp);
    }
    if exp >= 32 {
        return u32::MAX;
    }
    mant << (exp - 23)
}

struct MsgProc(usize);

impl MessageProcessor for MsgProc {
    fn process(&self, message: &[u8]) -> u32 {
        let offset = self.0;
        let offset = match *module_parameters::lux_offset.value() {
            0 => offset,
            v => v as usize,
        };
        if offset + 4 > message.len() {
            return 0;
        }
        let raw = u32::from_le_bytes(message[offset..offset + 4].try_into().unwrap());
        f32_to_u32(raw)
    }
}

#[repr(transparent)]
struct IIOAopAlsDriver(IIORegistration<MsgProc>);

kernel::of_device_table!(
    OF_TABLE,
    MODULE_OF_TABLE,
    (),
    [(of::DeviceId::new(c_str!("apple,aop-als")), ())]
);

impl platform::Driver for IIOAopAlsDriver {
    type IdInfo = ();

    const OF_ID_TABLE: Option<of::IdTable<Self::IdInfo>> = Some(&OF_TABLE);

    fn probe(pdev: &platform::Device<Core>, _info: Option<&()>) -> impl PinInit<Self, Error> {
        let dev = pdev.as_ref();
        let parent = dev.parent().unwrap();
        // SAFETY: our parent is AOP, and AopDriver is repr(transparent) for Arc<dyn Aop>
        let adata_ptr = unsafe { Pin::<KBox<Arc<dyn AOP>>>::borrow(parent.get_drvdata()) };
        let adata = (&*adata_ptr).clone();
        // SAFETY: AOP sets the platform data correctly
        let service = unsafe { *((*dev.as_raw()).platform_data as *const EPICService) };
        let ty = bindings::BINDINGS_IIO_LIGHT;
        // Recorded from the 2026-09-20 hypervisor trace, and deliberately NOT
        // replayed: macOS sends subtype 0x01, the HID report descriptor fetch,
        // as the very first call on this channel, where this driver sends the
        // 0x0f name query that picks the lux offset.  Every other byte of the
        // two 0x01 calls matched -- same channel (0x1000000a), EPIC type,
        // category, subtype, payload (4 bytes) and reply capacity (0xe4) --
        // so ordering was the only difference.  It is not the gate: the
        // calibration on setup-port endpoint 0x21 is, and the AOP never
        // answers 0x01 on the untrusted side, where an unanswered EPIC call
        // wedges the endpoint's call slot and costs the iio device entirely.
        let offset = get_lux_offset(adata.as_ref(), pdev, &service)?;
        let data = AopSensorData::new(dev.into(), ty, MsgProc(offset))?;
        adata.add_fakehid_listener(service, data.clone())?;
        enable_als(adata.as_ref(), pdev, &service)?;
        let info_mask = 1 << bindings::BINDINGS_IIO_CHAN_INFO_PROCESSED;
        Ok(IIOAopAlsDriver(IIORegistration::<MsgProc>::new(
            data,
            c"aop-sensors-als",
            ty,
            info_mask,
            &THIS_MODULE,
        )?))
    }
}

module_platform_driver! {
    type: IIOAopAlsDriver,
    name: "iio_aop_als",
    description: "AOP ambient light sensor driver",
    license: "Dual MIT/GPL",
    alias: ["platform:iio_aop_als"],
    firmware: ["apple/aop-als-cal.bin"],
    params: {
        lux_offset: u32 {
            default: 0,
            description: "Override the byte offset of the little-endian f32 lux value in the AOP report (0 = the part's known offset)",
        },
        batch_interval_us: u32 {
            default: 1,
            description: "ALS property 0x16, written before the report interval. Default is what macOS writes on this machine (1, not an interval at all) as traced under the hypervisor 2026-09-20; the AOP answers kIOReturnNotPermitted to macOS too and arms anyway",
        },
        raw_interval_us: u32 {
            default: 0,
            description: "ALS property 0x5c, written between 0x16 and the report interval. Default is what macOS writes on this machine (0), traced 2026-09-20; also answered NotPermitted for macOS",
        },
        set_verbosity: u32 {
            default: 6,
            description: "Write the ALS VERBOSITY property (0xe1). macOS writes 6 here on this machine (traced 2026-09-20, answered NotPermitted); 0xffffffff means leave it alone",
        },
        interval_us: u32 {
            default: 200000,
            description: "Reporting interval in microseconds written to ALS property 0. macOS writes 200000 on this machine and the 0xc4 reports follow at that rate",
        },
    },
}

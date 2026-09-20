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
/// (hypervisor trace, 2026-09-20); Linux never has.
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
const SPU_PKT_GET_PROPERTY: u16 = 0x0a;
const SPU_PKT_SET_PROPERTY: u16 = 0x04;
/* SpuProperty ids (AppleSPUInterface). */
const SPU_PROP_REPORT_INTERVAL_US: u32 = 0x00;
const SPU_PROP_DEVICE_NAME: u32 = 0x0f;
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
const RAW_CH0_OFFSET_CT817: usize = 0x08;
const RAW_CH1_OFFSET_CT817: usize = 0x0c;
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
    // Replay macOS's opening, captured from this exact machine under the
    // hypervisor on 2026-09-20 (ALS-STATE-2026-09-20.md, "THE macOS ALS
    // CONVERSATION, CAPTURED").  Before any calibration or rate write,
    // AppleSPUCT817 fetches the HID report descriptor and reads eight
    // properties.  Linux has never done any of it.
    //
    // Everything here is logged and nothing fails the probe: macOS itself is
    // answered kIOReturnNotPermitted on 0x5a, 0x3e, 0x11 and 0x3f and carries
    // straight on, so a refusal here is the expected reference behaviour and
    // not an error.  This is replay of traced traffic, not a subtype sweep --
    // the rule against those still stands.
    {
        // Reply is 20 bytes after the u32 status, and decodes as:
        //   06 00 ff  Usage Page (Vendor-defined 0xFF00)
        //   0a 04 00  Usage (0x0004)      a1 01  Collection (Application)
        //   15 00     Logical Minimum 0   26 ff 00  Logical Maximum 255
        //   75 08     Report Size 8       95 20  Report Count 32
        //   81 00     Input               c0     End Collection
        // i.e. exactly the 32-byte vendor report this driver already decodes.
        let mut buf = KVec::new();
        buf.resize(8, 0, GFP_KERNEL)?;
        match aop.epic_call_ret(svc, EPIC_SUBTYPE_GET_HID_DESCRIPTOR, &buf, 0xe4) {
            Ok((rc, d)) => {
                let n = core::cmp::min(d.len(), 32);
                dev_info!(dev.as_ref(), "als: HID descriptor (0x01) -> rc {:#x}, {} bytes {:?}\n",
                          rc, d.len(), &d[..n])
            }
            Err(e) => dev_info!(dev.as_ref(), "als: HID descriptor (0x01) -> error {:?}\n", e),
        }

        // macOS's order.  0x1e is AppleSPUALSLuxDriver:getOrientation and 0x2e
        // is getPlacement -- both answer 1 on the J700, which is what the
        // panel's own log prints ("AOP orientation = 1", "AOP placement = 1").
        for &(tag, len, name) in &[
            (0x5au32, 2usize, "UNK5A"),
            (0x5b, 2, "PRODUCT_ID"),
            (0x30, 4, "UNK30"),
            (0x3e, 0x40, "UNK3E"),
            (0x0f, 0x40, "NAME"),
            (0x113, 1, "UNK113"),
            (0x2e, 4, "PLACEMENT"),
            (0x1e, 4, "ORIENTATION"),
        ] {
            match get_aop_property(aop, svc, tag, len) {
                Ok((rc, d)) => {
                    let n = core::cmp::min(d.len(), 16);
                    dev_info!(dev.as_ref(), "als: probe {} ({:#x}) -> rc {:#x} {:?}\n",
                              name, tag, rc, &d[..n])
                }
                Err(e) => dev_info!(dev.as_ref(), "als: probe {} ({:#x}) -> error {:?}\n",
                                    name, tag, e),
            }
        }
    }

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
    // The AOP answers the calibration and interval writes with rc 0 and then
    // sends no reports at all.  macOS's ALS driver knows two more property keys
    // that we have never written -- MODE (0xd7) and 0xe4 -- so expose them, and
    // the verbosity key, as load-time knobs: one of them is what arms a part on
    // the `als-new-calibration-path` generation.  u32::MAX means "leave alone",
    // and a write that the AOP rejects is logged rather than failing the probe,
    // so a sweep can run without losing the iio device.
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
    try_prop(0xd7, *module_parameters::set_mode.value(), "MODE");
    try_prop(0xe4, *module_parameters::set_unke4.value(), "UNKE4");

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

    let rc = set_als_property(aop, svc, 0, &0u32.to_le_bytes())?;
    dev_info!(dev.as_ref(), "als: INTERVAL (0x0) = 0 (stop) -> rc {:#x}\n", rc);
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

fn spu_get_property(
    aop: &dyn AOP,
    svc: &EPICService,
    prop: u32,
    data_len: usize,
) -> Result<(u32, KVec<u8>)> {
    let mut buf = KVec::new();
    buf.resize(4, 0, GFP_KERNEL)?;
    buf[0..4].copy_from_slice(&prop.to_le_bytes());
    aop.epic_call_ret(svc, SPU_PKT_GET_PROPERTY, &buf, data_len)
}

fn spu_set_property(aop: &dyn AOP, svc: &EPICService, prop: u32, data: &[u8]) -> Result<u32> {
    let mut buf = KVec::new();
    buf.resize(4 + data.len(), 0, GFP_KERNEL)?;
    buf[0..4].copy_from_slice(&prop.to_le_bytes());
    buf[4..].copy_from_slice(data);
    aop.epic_call(svc, SPU_PKT_SET_PROPERTY, &buf)
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
        // The lux is a little-endian f32 whose position depends on the sensor
        // part.  On a part whose layout is not known yet, dumping the report
        // and the float each candidate offset would yield identifies it from
        // real light instead of guesswork.
        if *module_parameters::dump_report.value() != 0 {
            let mut plausible = [0u32; 16];
            let mut n = 0usize;
            let mut off = 0usize;
            while off + 4 <= message.len() && n < plausible.len() {
                let v = f32_to_u32(u32::from_le_bytes(
                    message[off..off + 4].try_into().unwrap(),
                ));
                if v > 0 && v < 100_000 {
                    plausible[n] = ((off as u32) << 20) | (v & 0xfffff);
                    n += 1;
                }
                off += 1;
            }
            pr_info!(
                "aop-als: report {} bytes: {:02x?}\n",
                message.len(),
                message
            );
            for i in 0..n {
                pr_info!(
                    "aop-als:   offset {} -> {} lux (plausible)\n",
                    plausible[i] >> 20,
                    plausible[i] & 0xfffff
                );
            }
        }
        // A CT817 report is exactly 32 bytes; macOS drops anything else
        // (cmp x0,#0x20 / b.ne).  Log the two raw channels alongside the lux so
        // a reading can be sanity-checked against real light: ch0/ch1 should
        // track the lux, and all-zero channels with non-zero lux would mean the
        // offsets are wrong rather than the sensor being dark.
        if message.len() == CT817_REPORT_LEN
            && RAW_CH1_OFFSET_CT817 + 4 <= message.len()
            && *module_parameters::dump_report.value() != 0
        {
            let ch0 = f32_to_u32(u32::from_le_bytes(
                message[RAW_CH0_OFFSET_CT817..RAW_CH0_OFFSET_CT817 + 4]
                    .try_into()
                    .unwrap(),
            ));
            let ch1 = f32_to_u32(u32::from_le_bytes(
                message[RAW_CH1_OFFSET_CT817..RAW_CH1_OFFSET_CT817 + 4]
                    .try_into()
                    .unwrap(),
            ));
            pr_info!("aop-als: CT817 raw ch0 {} ch1 {}\n", ch0, ch1);
        }
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
        // u64 so that channel 0 is expressible: the announce log shows the
        // endpoint-0x20 services arriving on channels 0, 1 and 2 while their
        // announce bodies advertise 0x10000000/3/4, and every service whose
        // calls work (als, aop-audio, aop-voicetrigger, aop-audprov) has
        // arrival == body.  u64::MAX is "off".
        let spu_probe_raw = *module_parameters::spu_probe.value();
        let spu_chan = spu_probe_raw as u32;
        if spu_probe_raw != 0xffffffffffffffff {
            // macOS AppleSPUCT817::probe gates on GetProperty(0x0F) == "CT817"
            // (0xfffffe0008e415d0) and then the sensor is armed by writing a
            // non-zero microsecond interval to property 0x00.  The earlier
            // sweep of this endpoint used the `als` service's [u32 zero,
            // u32 tag] payload; the SPU wants the bare property id, so it
            // answered nothing.  Ask with the right shape.
            let spu = EPICService {
                channel: spu_chan,
                endpoint: 0x20,
            };
            // NEVER sweep unknown subtypes here: an unimplemented subtype
            // never answers and one unanswered call wedges the endpoint until
            // reboot (0x00..0x1f sweep, 2026-09-20).  Only subtype 0x0a (GET)
            // and 0x04 (SET) are known to reply on this endpoint.
            //
            // macOS writes the rate triple 0x16/0x5c/0x00 from
            // AppleSPUHIDDriver::setPropertiesGated at EL1 -- ordinary kernel
            // code, not GL and not the exclave -- and it reaches them through
            // AppleSPUInterface::SetProperty, i.e. THE SPU, not the AOP `als`
            // EPIC service.  Writing them to `als` returns 0xe00002e2
            // kIOReturnNotPermitted, which is very likely "not this service's
            // property" rather than a privilege refusal: the same service
            // accepts 0x00 and 0x0b happily.  So send them where macOS sends
            // them.  _spuSetPropertyGated's payload is le32 prop_id followed by
            // the value (inLen = 4 + len).
            let spu = EPICService {
                channel: spu_chan,
                endpoint: 0x20,
            };
            // SET (0x04) on this channel answers 0xe00002c2 kIOReturnBadArgument,
            // NOT 0xe00002c7 kIOReturnUnsupported -- so the operation exists on
            // the SPU and only the payload shape is wrong.  (GET 0x0a is the
            // other way round: Unsupported with the 8-byte envelope.)  Sweep
            // the plausible encodings for one property and report each rc, so
            // the accepted shape can be identified in a single boot.
            let batch = *module_parameters::batch_interval_us.value();
            let prop = 0x16u32;
            let mut shapes: KVec<(&'static str, KVec<u8>)> = KVec::new();
            {   // [prop, value] -- macOS _spuSetPropertyGated inline form
                let mut b: KVec<u8> = KVec::new();
                b.extend_from_slice(&prop.to_le_bytes(), GFP_KERNEL)?;
                b.extend_from_slice(&batch.to_le_bytes(), GFP_KERNEL)?;
                shapes.push(("[prop,val]", b), GFP_KERNEL)?;
            }
            {   // [0, prop, value] -- the AOP `als` service's envelope
                let mut b: KVec<u8> = KVec::new();
                b.extend_from_slice(&0u32.to_le_bytes(), GFP_KERNEL)?;
                b.extend_from_slice(&prop.to_le_bytes(), GFP_KERNEL)?;
                b.extend_from_slice(&batch.to_le_bytes(), GFP_KERNEL)?;
                shapes.push(("[0,prop,val]", b), GFP_KERNEL)?;
            }
            {   // [prop, 0, value]
                let mut b: KVec<u8> = KVec::new();
                b.extend_from_slice(&prop.to_le_bytes(), GFP_KERNEL)?;
                b.extend_from_slice(&0u32.to_le_bytes(), GFP_KERNEL)?;
                b.extend_from_slice(&batch.to_le_bytes(), GFP_KERNEL)?;
                shapes.push(("[prop,0,val]", b), GFP_KERNEL)?;
            }
            {   // [prop, value] with a 2-byte value (the int16 ms form)
                let mut b: KVec<u8> = KVec::new();
                b.extend_from_slice(&prop.to_le_bytes(), GFP_KERNEL)?;
                b.extend_from_slice(&((batch / 1000) as u16).to_le_bytes(), GFP_KERNEL)?;
                shapes.push(("[prop,u16]", b), GFP_KERNEL)?;
            }
            {   // value only, no property id
                let mut b: KVec<u8> = KVec::new();
                b.extend_from_slice(&batch.to_le_bytes(), GFP_KERNEL)?;
                shapes.push(("[val]", b), GFP_KERNEL)?;
            }
            for (label, buf) in shapes.iter() {
                match adata
                    .as_ref()
                    .epic_call(&spu, SPU_PKT_SET_PROPERTY, buf)
                {
                    Ok(rc) => pr_info!(
                        "aop-als: SPU ch {:#x} SET 0x16 {} ({} bytes) -> rc {:#x}\n",
                        spu_chan, label, buf.len(), rc
                    ),
                    Err(e) => pr_info!(
                        "aop-als: SPU ch {:#x} SET 0x16 {} ({} bytes) ERR {:?}\n",
                        spu_chan, label, buf.len(), e
                    ),
                }
            }
            let us = *module_parameters::spu_interval_us.value();
            if us != 0 {
                match spu_set_property(
                    adata.as_ref(),
                    &spu,
                    SPU_PROP_REPORT_INTERVAL_US,
                    &us.to_le_bytes(),
                ) {
                    Ok(rc) => pr_info!(
                        "aop-als: SPU ch {:#x} report interval {} us -> rc {:#x}\n",
                        spu_chan, us, rc
                    ),
                    Err(e) => pr_info!(
                        "aop-als: SPU ch {:#x} report interval {} us failed: {:?}\n",
                        spu_chan, us, e
                    ),
                }
            }
        }
        if *module_parameters::dump_props.value() != 0 {
            // The AOP accepts the enable but delivers no reports on this
            // machine.  Ask it what it actually exposes: property 0xf is the
            // part name, and the neighbours carry the sampling state and, on a
            // generation that holds its own calibration, the calibration too.
            // Sweep the whole 8-bit tag space, not just the low half: the
            // ALS property keys macOS uses (MODE 0xd7, VERBOSITY 0xe1, 0xe4)
            // all live above 0x80 and had never been read.  Ask for a large
            // buffer too -- a CT720-shaped calibration is 145 bytes, so a
            // 64-byte request would have truncated or missed one.
            let plen = *module_parameters::prop_len.value() as usize;
            for tag in 0u32..0x100 {
                match get_aop_property(adata.as_ref(), &service, tag, plen) {
                    Ok((rc, v)) if rc == 0 || !v.is_empty() => {
                        let n = core::cmp::min(v.len(), 160);
                        pr_info!(
                            "aop-als: property {:#04x} -> rc {:#x}, {} bytes: {:02x?}\n",
                            tag,
                            rc,
                            v.len(),
                            &v[..n]
                        );
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        }
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
        prop_len: u32 {
            default: 256,
            description: "Bytes to request for each property during the dump_props sweep",
        },
        spu_probe: u64 {
            default: 0xffffffffffffffff,
            description: "DANGEROUS, default off. EPIC channel on AOP endpoint 0x20 to address the SPU for CT817 property calls. An EPIC call to a channel the AOP does not recognise is never answered, and the unanswered call wedges this ASC: verified 2026-09-20 that a probe on channel 0x10000000 timed out and then broke aop_audio (snd_soc_dai_startup on spkr, -110) until a reboot. Do not set this on a machine whose audio is in use, and confirm the channel from the announce log first",
        },
        spu_interval_us: u32 {
            default: 0,
            description: "With spu_probe set and the CT817 identified, write this microsecond report interval to SPU property 0x00 to arm periodic reporting (macOS defaults: 50000/100000/200000, or 10000 for a client that does not ask); 0 only identifies the part",
        },
        batch_interval_us: u32 {
            default: 500000,
            description: "SPU property 0x16 BatchInterval in microseconds, written before the report interval exactly as AppleSPUHIDDriver::setPropertiesGated does; CT817's own slow-rate pairing is report 200000 with batch 500000",
        },
        raw_interval_us: u32 {
            default: 200000,
            description: "SPU property 0x5c RawInterval in microseconds, written between the batch and report intervals",
        },
        dump_props: u8 {
            default: 0,
            description: "Enumerate the AOP ALS EPIC properties at probe and log each reply, to find why the sensor is enabled but sends no reports",
        },
        dump_report: u8 {
            default: 0,
            description: "Log each AOP ALS report and every offset that decodes to a plausible lux value, to identify the layout of an unknown part",
        },
        set_mode: u32 {
            default: 0xffffffff,
            description: "Write the ALS MODE property (0xd7) with this value before the interval; 0xffffffff leaves it alone",
        },
        set_unke4: u32 {
            default: 0xffffffff,
            description: "Write the ALS property 0xe4 with this value before the interval; 0xffffffff leaves it alone",
        },
        set_verbosity: u32 {
            default: 0xffffffff,
            description: "Write the ALS VERBOSITY property (0xe1); macOS sends 0xffffffff here, and 0xffffffff means leave it alone",
        },
        interval_us: u32 {
            default: 200000,
            description: "Reporting interval in microseconds written to ALS property 0",
        },
    },
}

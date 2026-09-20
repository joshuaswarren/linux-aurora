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

/// macOS's opening, in macOS's position: **before any property read**.
///
/// The 2026-09-20 wire dump showed the one difference that was left.  macOS
/// sends subtype 0x01, the HID report descriptor fetch, as the very first
/// call on the ALS channel.  This driver sent a property read first -- the
/// 0x0f name query that picks the lux offset -- and only then the 0x01, which
/// the firmware never answered.  Every byte of the two 0x01 calls matched
/// otherwise: same channel (0x1000000a), EPIC type, category, subtype,
/// payload (4 bytes) and reply capacity (0xe4).  Order was the difference.
fn replay_macos_open(aop: &dyn AOP, dev: &platform::Device, svc: &EPICService) -> Result<()> {
    // Replay macOS's opening, captured from this machine under the hypervisor
    // on 2026-09-20 (ALS-STATE-2026-09-20.md, "THE macOS ALS CONVERSATION,
    // CAPTURED").  Before any calibration or rate write, AppleSPUCT817 fetches
    // the HID report descriptor and reads eight properties.
    //
    // Refusals are expected: macOS is answered kIOReturnNotPermitted on 0x5a,
    // 0x3e, 0x11 and 0x3f and carries straight on.  A *timeout* is not -- an
    // unanswered EPIC call wedges the endpoint, and the first attempt at this
    // replay proved it by sending the wrong payload length (see below), after
    // which all nine calls and the enable itself came back ETIMEDOUT and probe
    // failed with no iio device at all.  So: stop at the first unanswered
    // call, and keep the whole thing behind a parameter that can be cleared
    // without a rebuild.
    if *module_parameters::replay_open.value() != 0 {
        // The payload here is FOUR bytes, not the eight a property call takes.
        // Read the traced sub-header rather than the call next to it:
        //     0x18: 04 00 00 00   sub.length = 4
        //     0x2c: e4 00 00 00   reply capacity
        //     0x30: 00 00 00 00   payload
        // The reply is 20 bytes after the u32 status and decodes as
        //   06 00 ff  Usage Page (Vendor-defined 0xFF00)
        //   0a 04 00  Usage (0x0004)      a1 01  Collection (Application)
        //   15 00     Logical Minimum 0   26 ff 00  Logical Maximum 255
        //   75 08     Report Size 8       95 20  Report Count 32
        //   81 00     Input               c0     End Collection
        // i.e. exactly the 32-byte vendor report this driver already decodes.
        let mut buf = KVec::new();
        buf.resize(4, 0, GFP_KERNEL)?;
        let hid_ok = match aop.epic_call_ret(svc, EPIC_SUBTYPE_GET_HID_DESCRIPTOR, &buf, 0xe4) {
            Ok((rc, d)) => {
                let n = core::cmp::min(d.len(), 32);
                dev_info!(dev.as_ref(), "als: HID descriptor (0x01) -> rc {:#x}, {} bytes {:?}\n",
                          rc, d.len(), &d[..n]);
                true
            }
            Err(e) => {
                dev_warn!(dev.as_ref(),
                          "als: HID descriptor (0x01) -> error {:?}; skipping the rest of the \
                           macOS opening so a wedged endpoint is not hammered\n", e);
                false
            }
        };

        // macOS's order.  0x1e is AppleSPUALSLuxDriver:getOrientation and 0x2e
        // is getPlacement -- both answer 1 here, which is what the panel's own
        // log prints ("AOP orientation = 1", "AOP placement = 1").
        const PROBE_PROPS: [(u32, usize, &str); 8] = [
            (0x5a, 2, "UNK5A"),
            (0x5b, 2, "PRODUCT_ID"),
            (0x30, 4, "UNK30"),
            (0x3e, 0x40, "UNK3E"),
            (0x0f, 0x40, "NAME"),
            (0x113, 1, "UNK113"),
            (0x2e, 4, "PLACEMENT"),
            (0x1e, 4, "ORIENTATION"),
        ];
        let props: &[(u32, usize, &str)] = if hid_ok { &PROBE_PROPS } else { &[] };
        for &(tag, len, name) in props {
            match get_aop_property(aop, svc, tag, len) {
                Ok((rc, d)) => {
                    let n = core::cmp::min(d.len(), 16);
                    dev_info!(dev.as_ref(), "als: probe {} ({:#x}) -> rc {:#x} {:?}\n",
                              name, tag, rc, &d[..n])
                }
                Err(e) => {
                    dev_warn!(dev.as_ref(), "als: probe {} ({:#x}) -> error {:?}; stopping\n",
                              name, tag, e);
                    break;
                }
            }
        }
    }
    Ok(())
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

    // macOS does NOT write 0x00 = 0 before the enable.  The "0 -> non-zero
    // transition of 0x00 is what arms it" reading came from disassembling
    // setPropertiesGated; the live trace supersedes it -- macOS writes
    // 0xe1 = 6, 0x16 = 1, 0x5c = 0, then 0x00 = 200000, and the reports start.
    // Keep the stop write available, off by default, in case a re-arm after a
    // previous session needs it.
    if *module_parameters::stop_first.value() != 0 {
        let rc = set_als_property(aop, svc, 0, &0u32.to_le_bytes())?;
        dev_info!(dev.as_ref(), "als: INTERVAL (0x0) = 0 (stop) -> rc {:#x}\n", rc);
    }
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
        // Before anything else on this channel: see replay_macos_open().
        replay_macos_open(adata.as_ref(), pdev, &service)?;
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
        batch_interval_us: u32 {
            default: 1,
            description: "ALS property 0x16, written before the report interval. Default is what macOS writes on this machine (1, not an interval at all) as traced under the hypervisor 2026-09-20; the AOP answers kIOReturnNotPermitted to macOS too and arms anyway",
        },
        raw_interval_us: u32 {
            default: 0,
            description: "ALS property 0x5c, written between 0x16 and the report interval. Default is what macOS writes on this machine (0), traced 2026-09-20; also answered NotPermitted for macOS",
        },
        dump_props: u8 {
            default: 0,
            description: "Enumerate the AOP ALS EPIC properties at probe and log each reply, to find why the sensor is enabled but sends no reports",
        },
        dump_report: u8 {
            default: 0,
            description: "Log each AOP ALS report and every offset that decodes to a plausible lux value, to identify the layout of an unknown part",
        },
        set_verbosity: u32 {
            default: 6,
            description: "Write the ALS VERBOSITY property (0xe1). macOS writes 6 here on this machine (traced 2026-09-20, answered NotPermitted); 0xffffffff means leave it alone",
        },
        replay_open: u8 {
            default: 0,
            description: "Replay macOS's opening before enabling: fetch the HID report descriptor (subtype 0x01) and read the eight properties AppleSPUCT817 reads, as traced under the hypervisor 2026-09-20. OFF by default: the AOP never answers 0x01 on the untrusted side -- the ALS frontend it would reach is a property surface, while CT817Hal, ALSCT817 and ALSTrustedClientManager are trusted-side (see ALS-STATE-2026-09-20.md) -- and an unanswered EPIC call wedges the endpoint's call slot, which costs the iio device entirely. Set it to 1 only to re-measure that",
        },
        stop_first: u8 {
            default: 0,
            description: "Write ALS property 0x00 = 0 before the enable. macOS does not do this; off by default since the trace of 2026-09-20 superseded the transition theory it was added for",
        },
        interval_us: u32 {
            default: 200000,
            description: "Reporting interval in microseconds written to ALS property 0. macOS writes 200000 on this machine and the 0xc4 reports follow at that rate",
        },
    },
}

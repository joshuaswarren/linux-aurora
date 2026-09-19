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
const LUX_OFFSET_CT720: usize = 0x1d;
const LUX_OFFSET_VD6286: usize = 0x28;

fn get_lux_offset(aop: &dyn AOP, dev: &platform::Device, svc: &EPICService) -> Result<usize> {
    let name = get_aop_property(aop, svc, 0xf, 16)?.1;
    match name.as_slice() {
        b"Redbird\0" => Ok(LUX_OFFSET_VD6286),
        b"FireFish2\0" => Ok(LUX_OFFSET_CT720),
        // J700 (MacBook Neo): the AOP reports the part number itself rather
        // than a codename.  CT817 is the same family as the CT720 behind
        // "FireFish2", so it takes that report layout; the offset is a
        // hypothesis until a reading has been checked against real light.
        b"CT817\0" => Ok(LUX_OFFSET_CT720),
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

    let ivl = *module_parameters::interval_us.value();
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

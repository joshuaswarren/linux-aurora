// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! G18 compute register serialization, derived from the original 26A428 HAL400.
//!
//! This is the register portion of the 0x1020-byte command, not a complete kick.
//! The caller must own and validate all GPU addresses before publication. Native
//! offsets retained in field names are deliberately not assigned guessed meanings.
//! Register overrides, optional SMAPP preemption objects and random/alternating
//! tuning policies are outside this initial contract.

#![allow(dead_code)]

pub(crate) const G18_COMPUTE_COMMAND_BYTES: usize = 0x1020;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct G18ComputeRegisterInputs {
    pub command_gpu: u64,
    /// Native descriptor +4d8, +480, +4e8, +4f0, +4f8, +500, in emission order.
    pub program: [u64; 6],
    /// Native descriptor +578 through +5a0, in emission order.
    pub aliases: [u64; 6],
    pub d550: u64,
    pub d558: u64,
    pub d548: u64,
    pub d5a8: u64,
    pub d5b0: u64,
    pub d540: u8,
    pub d5d8: u8,
    pub d5d9: u8,
    pub d62a: u8,
    pub d62b: u8,
    pub d420: u8,
    pub d418: u32,
    pub d774: u32,
    pub d5c0: u32,
    pub d5c4: u32,
    pub d5c8: u32,
    pub d50c: u32,
    pub d510: u32,
    pub d514: u32,
    pub d630: u32,
    pub context_id: u32,
    pub pool_slot: u32,
    pub pool_generation: u64,
    pub operands: [u64; 2],
    pub support_control: u64,
    pub clusters: u32,
    pub cluster_width: u32,
    pub a6b7: u8,
    pub a6b9: u8,
    pub a6d0: u8,
    pub a758: u32,
    pub a10aac: u64,
    pub a10ab4: u64,
    pub a10aec: u32,
    pub a10af0: u8,
    pub a10af4: u32,
    pub a10b04: u8,
    pub a10b08: u32,
    pub a10b18: u32,
    pub policy54: u8,
    pub policy2c: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum G18ComputeRegisterError {
    InvalidTopology,
    AddressOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct G18ComputeRegisterSummary {
    pub main_gpu: u64,
    pub main_count: u16,
    pub total_main_count: u16,
    pub secondary_gpu: u64,
    pub secondary_count: u16,
}

fn put16(out: &mut [u8], offset: usize, value: u16) {
    out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read32(out: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(out[offset..offset + 4].try_into().unwrap())
}

fn append(out: &mut [u8], base: usize, count: &mut u16, number: u32, value: u64) {
    let offset = base + usize::from(*count) * 12;
    let header = (read32(out, offset) & 0xfffc0006) | (number & 0x3fff9);
    put32(out, offset, header);
    put64(out, offset + 4, value);
    *count += 1;
}

/// Fill only native generateRegisterList outputs; leave all other command bytes
/// untouched. The full-command builder sets command+0xf68 before this call.
pub(crate) fn encode_g18_compute_registers(
    out: &mut [u8; G18_COMPUTE_COMMAND_BYTES],
    p: &G18ComputeRegisterInputs,
) -> Result<G18ComputeRegisterSummary, G18ComputeRegisterError> {
    if p.clusters == 0 || p.clusters > 16 || p.cluster_width == 0 || p.cluster_width > 16 {
        return Err(G18ComputeRegisterError::InvalidTopology);
    }
    let main_gpu = p
        .command_gpu
        .checked_add(0x40)
        .ok_or(G18ComputeRegisterError::AddressOverflow)?;
    let secondary_gpu = p
        .command_gpu
        .checked_add(0x760)
        .ok_or(G18ComputeRegisterError::AddressOverflow)?;
    let mode_table = [
        0x154006201u64,
        0x154004201,
        0x154002201,
        0x154002201,
        0x154002201,
        0x154002201,
        0x154000201,
    ];
    let mut control = 0x154024200u64;
    if p.a10b04 & 1 != 0 {
        control = mode_table
            .get(p.a10b08.wrapping_sub(1) as usize)
            .copied()
            .unwrap_or(0x154002201);
        control = (control | (u64::from(p.a10b18) << 17)) & 0x1ffffffee6200;
    }
    let cs_mode = if p.a10af0 & 1 == 0 {
        p.a10aec
    } else {
        u32::from(p.a10af4 == 0)
    };
    control = ((p.a10aac >> 28 | p.a10aac << 9) & 0x1e1fffffe30)
        | ((control | u64::from(cs_mode) << 20) & 0xfffe1fffe6001)
        | u64::from((!p.d540) & 1)
        | ((p.a10ab4 & 0x87ffffff) << 6)
        | ((p.a10ab4 >> 32) << 33);
    if p.a6d0 == 1 {
        control |= 0x30;
    }
    let mut count = 0u16;
    macro_rules! emit {
        ($number:expr, $value:expr) => {
            append(out, 0x40, &mut count, $number, $value)
        };
    }
    if p.d5d8 & 1 != 0 {
        emit!(0x81e9, 1);
    }
    if p.d5d9 & 1 != 0 {
        emit!(0x81f9, 1);
    }
    for (number, value) in [0x1a510, 0x1a420, 0x1a4d0, 0x1a4d8, 0x1a4e0, 0x1a4e8]
        .into_iter()
        .zip(p.program)
    {
        emit!(number, value);
    }
    emit!(0x1a440, control);
    put64(out, 0xf08, control);
    emit!(0xa5e1, p.d550 | p.d558 << 16);
    if p.d548 != 0 {
        emit!(0xa0f1, p.d548 & !0xfff);
    }
    let mut mode = if p.d62a & 1 == 0 {
        0x10c08860
    } else {
        0x14c08a60
    };
    if p.d62b & 1 != 0 {
        mode |= 0x80;
    }
    if p.clusters > 2 || p.a6b9 & 1 != 0 || p.d548 != 0 {
        mode |= 0x1c;
    }
    emit!(0x1a458, mode);
    emit!(0x101d9, u64::from(p.d774 & 0x7f));
    emit!(0xa111, 0xffff00000000);
    emit!(0xa131, 0x7000000000);
    emit!(0xa151, 0x7100000000);
    for (number, value) in [0x1a079, 0x1a081, 0x1a049, 0x1a051, 0x1a0a9, 0x1a0b1]
        .into_iter()
        .zip(p.aliases)
    {
        emit!(number, value);
    }
    emit!(0x101d1, (p.d5a8 & 0xffffffffc00fffff) | p.d5b0 << 20);
    emit!(0x1a0d9, u64::from(p.d5c4 & 0xfffffff8));
    let next = p.d5c8.wrapping_add(1);
    emit!(0x1a109, u64::from(if next < 32 { next } else { 0 }));
    let mut dispatch = if p.d5d8 & 1 == 0 {
        0xff0004
    } else {
        (u64::from(p.d5c0) << 16) | 0x1000004
    };
    if p.a6b7 & 1 == 0 {
        dispatch = (dispatch & 0xfffffffb) | (u64::from(p.a758) << 32);
    } else {
        dispatch &= 0xffffffff;
    }
    dispatch |= u64::from(p.d5d9 & 1) << 25;
    emit!(0x107a1, dispatch);
    put64(out, 0x1006, dispatch);
    put32(out, 0x100e, 0);
    let identity = u64::from(read32(out, 0xf68)) | u64::from(p.d50c) << 32;
    emit!(0x1a540, identity);
    emit!(0x11209, identity);
    emit!(0xa351, identity);
    if p.policy54 & 1 != 0 {
        if p.policy2c & 8 != 0 {
            emit!(0x1a430, 0x3c0f);
        }
        let dimensions = p.d510.min(0x3fff) | p.d514.min(0x3fff) << 16;
        for number in [0x1a4f0, 0x1a4f8, 0x1a500, 0x1a508] {
            emit!(number, u64::from(dimensions));
        }
    }
    put32(out, 0x20, 0);
    let mode = if p.d420 & 1 == 0 {
        0
    } else if p.d418 == 1 {
        3
    } else {
        1
    };
    emit!(0xa731, mode);
    let context = u64::from((p.d630 & 0x3f) | p.context_id.wrapping_shl(8));
    emit!(0x10201, context);
    emit!(0x10428, context);
    emit!(
        0x10241,
        (p.pool_slot as i32 as i64 as u64) | p.pool_generation << 16
    );
    emit!(0xa611, 0);
    emit!(
        0x10229,
        (p.support_control & 0x7ffffffffc0) | 0x1000000000000000
    );
    if p.d5d8 & 1 != 0 {
        emit!(0x81e1, 1);
    }
    if p.d5d9 & 1 != 0 {
        emit!(0x81f1, 1);
    }
    let extent = u64::from(p.cluster_width + 10 * p.clusters - 10) * 0x1800;
    let a = p.operands[0] & 0x7fffffffc00;
    let b = p.operands[1] & 0x7fffffffc00;
    emit!(0x10099, a | 5);
    emit!(0x10091, a + extent);
    emit!(0xa5c1, b | 5);
    emit!(0xa5c9, b + extent);
    let main_count = count;
    put16(out, 0x18, main_count);
    emit!(0xa731, 0);
    emit!(0x1a440, control | 8);
    put16(out, 0x1a, count);
    put16(out, 0x1c, count);
    put64(out, 0x740, main_gpu);
    put16(out, 0x748, count);
    let mut secondary_count = 0;
    for (number, value) in [
        (0x10099, b | 5),
        (0x10091, b + extent),
        (0xa5c1, a | 5),
        (0xa5c9, a + extent),
    ] {
        append(out, 0x760, &mut secondary_count, number, value);
    }
    put64(out, 0xe60, secondary_gpu);
    put16(out, 0xe68, secondary_count);
    Ok(G18ComputeRegisterSummary {
        main_gpu,
        main_count,
        total_main_count: count,
        secondary_gpu,
        secondary_count,
    })
}

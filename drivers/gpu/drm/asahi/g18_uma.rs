// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! T8152 / 26A428 HAL400 UMA hardware descriptor serialization.
//!
//! Native `halLoadUMADesc` at 0xfffffe0008c40980 writes sixteen u64s per
//! hardware slot (0x80 bytes). This differs from M5's four-word format.
//! The caller must supply the owned firmware pool state and the four
//! device configuration words, and validate all addresses and lifetimes.
//! Encoding parity does not establish a usable pool or execute GPU work.

#![allow(dead_code)]

pub(crate) const G18_UMA_STATE_BYTES: usize = 0x7c;
pub(crate) const G18_UMA_DESCRIPTOR_BYTES: usize = 0x80;

/// Native `halInitSMAEnableMask` at 0xfffffe0008c41bdc splits the active
/// 128-bit mask into sixteen groups, each stored in a 16-bit output lane.
/// The primary mask is accelerator+0x470; +0x480 is used only when it is zero.
/// `group_width` is accelerator+0x4ec. This helper admits widths fitting the
/// output lanes; wider native profiles need a separate qualified contract.
pub(crate) fn pack_g18_sma_masks(
    primary: [u64; 2],
    fallback: [u64; 2],
    group_width: u32,
) -> Option<[u64; 4]> {
    if !(1..=16).contains(&group_width) {
        return None;
    }
    let selected = if primary == [0; 2] { fallback } else { primary };
    let mut remaining = u128::from(selected[0]) | (u128::from(selected[1]) << 64);
    let mask = (1u128 << group_width) - 1;
    let mut words = [0; 4];
    for group in 0..16 {
        words[group / 4] |= ((remaining & mask) as u64) << ((group % 4) * 16);
        remaining >>= group_width;
    }
    Some(words)
}

fn read64(state: &[u8; G18_UMA_STATE_BYTES], offset: usize) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&state[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

fn read32(state: &[u8; G18_UMA_STATE_BYTES], offset: usize) -> u32 {
    let mut bytes = [0; 4];
    bytes.copy_from_slice(&state[offset..offset + 4]);
    u32::from_le_bytes(bytes)
}

/// Pack the exact M6 hardware format. Preserve native masks and truncation;
/// do not substitute M5 directory geometry or infer opaque state values.
///
/// `device_config` is native accelerator+0x11af8..0x11b10. These four
/// values are supplied by the caller; zero is not a qualified M6 default.
pub(crate) fn pack_g18_uma_descriptor(
    state: &[u8; G18_UMA_STATE_BYTES],
    device_config: [u64; 4],
) -> [u64; G18_UMA_DESCRIPTOR_BYTES / 8] {
    [
        (read64(state, 0x1c) << 40 & 0xfffc_0000_0000_0000) | (read64(state, 0x14) >> 10),
        read64(state, 0x24),
        read64(state, 0x2c),
        device_config[0],
        device_config[1],
        device_config[2],
        device_config[3],
        read64(state, 0x34) << 7,
        read64(state, 0x3c) << 7,
        read64(state, 0x44),
        (u64::from(read32(state, 0x58)) << 44) | (read64(state, 0x4c) >> 6),
        u64::from(read32(state, 0x54)),
        read64(state, 0x5c),
        read64(state, 0x64),
        read64(state, 0x6c),
        read64(state, 0x74),
    ]
}

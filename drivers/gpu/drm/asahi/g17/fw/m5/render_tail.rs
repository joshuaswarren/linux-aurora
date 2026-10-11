// SPDX-License-Identifier: GPL-2.0-only
//! M5 render descriptor tail encodings.

#[derive(Clone, Copy)]
pub(crate) enum RenderStage { Tiling, Fragment }

/// The context tag packs a six-bit execution identity above its reuse
/// generation. All 314 native M5 render records agree with their header ID.
pub(crate) fn context_token(context: u32, generation: u64) -> Option<u64> {
    if context >= 64 || generation == 0 || generation >= (1u64 << 58) {
        return None;
    }
    Some((u64::from(context) << 58) | generation)
}

pub(crate) struct RenderTail {

    pub(crate) descriptor_value: u64,

    pub(crate) resolved_value: Option<u64>,

    pub(crate) trailing_value: u64,
}

pub(crate) fn encode(out: &mut [u8], stage: RenderStage, value: &RenderTail) -> Option<()> {
    let base = match stage { RenderStage::Tiling => 0x937, RenderStage::Fragment => 0x21d1 };
    let tail = out.get_mut(base..base + 24)?;
    tail[..8].copy_from_slice(&value.descriptor_value.to_le_bytes());
    if let Some(resolved) = value.resolved_value {
        tail[8..16].copy_from_slice(&resolved.to_le_bytes());
    }
    tail[16..24].copy_from_slice(&value.trailing_value.to_le_bytes());
    Some(())
}

pub(crate) struct ExtraAddresses {
    pub(crate) count: u32,
    pub(crate) policy: u8,
    pub(crate) addresses: [u64; 4],
}

pub(crate) fn encode_extra(out: &mut [u8], stage: RenderStage, value: &ExtraAddresses) -> Option<()> {
    let (base, end) = match stage {
        RenderStage::Tiling => (0x96f, 0x990),
        RenderStage::Fragment => (0x220e, 0x2233),
    };
    let tail = out.get_mut(base..end)?;
    tail[32] &= 0xf2;
    if value.count != 0 || value.addresses.iter().any(|&address| address != 0) {
        tail[32] |= 1 | ((value.policy & 3) << 2);
        for (slot, address) in tail[..32].chunks_exact_mut(8).zip(value.addresses) {
            slot.copy_from_slice(&address.to_le_bytes());
        }
    }
    if let RenderStage::Fragment = stage { tail[33..37].fill(0); }
    Some(())
}

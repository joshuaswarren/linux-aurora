// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! Evidence-backed G17 three-level firmware init-data layout.
//!
//! The M5 firmware consumes one root per physical firmware role. Each
//! 0xc8-byte root points at a 0x4c0-byte runtime-pointer table, which in turn
//! points at a 0x2828-byte hardware/globals object. This module encodes only
//! fields whose offsets and values are grounded by matching host writes and
//! firmware reads. Unknown configuration, power, channel and queue fields stay
//! zero, so this is deliberately not a bootable init-data producer yet.
//!
//! Byte slices are used instead of packed Rust structures because two proven
//! runtime-pointer fields are unaligned (`+0x254` and `+0x481`).

use core::result::Result;

/// The recovered fields are sufficient for offline validation and encoding.
pub(crate) const GROUNDED_LAYOUT_AVAILABLE: bool = true;
/// Unknown configuration, channel, queue and power fields still make the
/// generated object graph unsafe to hand to firmware.
pub(crate) const BOOTABLE_LAYOUT_AVAILABLE: bool = false;

pub(crate) const ROOT_SIZE: usize = 0x0c8;
pub(crate) const RUNTIME_POINTERS_SIZE: usize = 0x4c0;
pub(crate) const HW_GLOBALS_SIZE: usize = 0x2828;
pub(crate) const INITDATA_MAGIC: u64 = 0x0c8d_e339_0728_04c0;
pub(crate) const PERF_STATE_MAX: usize = 16;

const INITDATA_ADDR_MASK: u64 = 0x0000_0fff_ffff_ffff;
const INITDATA_ADDR_PREFIX: u64 = 0xffff_f000_0000_0000;

const ROOT_FW_SHARED_ALLOC: usize = 0x08;
const ROOT_RUNTIME_POINTERS: usize = 0x18;
const ROOT_SHARED: usize = 0x20;
const ROOT_ROLE: usize = 0x28;
const ROOT_HOST_MAPPED_ALLOCS: usize = 0x2c;
const ROOT_GPU_CONFIG: core::ops::Range<usize> = 0x30..0x98;
const ROOT_ROLE_POINTER: usize = 0xa8;
const ROOT_ROLE0_CROSS: usize = 0xb0;
const ROOT_ROLE1_CROSS: usize = 0xb8;
const ROOT_ROLE1_CROSS_2: usize = 0xc0;

const RT_HW_GLOBALS: usize = 0x000;
const RT_CHANNEL_DESCRIPTORS: usize = 0x020;
const RT_CHANNEL_DESCRIPTOR_STRIDE: usize = 0x20;
// The proved byte range ends at 0x19f. A claimed thirteenth descriptor would
// alias the independently proved cursor at 0x1a0, so only twelve are exposed.
pub(crate) const RT_CHANNEL_DESCRIPTOR_COUNT: usize = 12;
const RT_ROLE_POINTER: usize = 0x200;

const HW_PERF_STATE0_CLOCK_MHZ: usize = 0x10b8;
const HW_PERF_STATE_CLOCK_MHZ: usize = 0x10bc;
const HW_PERF_STATE_COUNT_MINUS_ONE: usize = 0x1b3c;

/// Physical firmware role encoded at root `+0x28`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum FirmwareRole {
    Gfx = 0,
    Gfx1 = 1,
}

/// GPU virtual addresses consumed by the grounded subset for one role.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct RoleAddresses {
    /// Address of this role's root. It is validated for the 44-bit doorbell.
    pub(crate) root: u64,
    pub(crate) runtime_pointers: u64,
    pub(crate) hw_globals: u64,
    /// Shared firmware allocation. Zero is allowed because only GFX1 reads it.
    pub(crate) firmware_shared_allocation: u64,
    pub(crate) shared: u64,
    pub(crate) role: u64,
    pub(crate) cross_role: u64,
    /// Proven only for GFX1 and required there; must be zero for GFX.
    pub(crate) cross_role_2: u64,
}

/// Fail-closed validation errors for the grounded layout subset.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum InitdataError {
    BufferSize,
    AddressOutOfRange,
    NullRequiredAddress,
    RoleSpecificAddress,
    PairSharedPointerMismatch,
    PairAliasedRoot,
    PairAliasedRuntimePointers,
    PerfStateCount,
    InterfaceMismatch,
    HostMappedAllocationsDisabled,
}

/// Firmware allocations whose sizes and runtime-pointer destinations are
/// recovered exactly. Their contents are not implemented by this module.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct FirmwareAllocation {
    pub(crate) runtime_pointer_offset: usize,
    pub(crate) size: usize,
    pub(crate) role_only: Option<FirmwareRole>,
}

pub(crate) const FIRMWARE_ALLOCATIONS: [FirmwareAllocation; 8] = [
    FirmwareAllocation {
        runtime_pointer_offset: 0x000,
        size: HW_GLOBALS_SIZE,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x254,
        size: 0x0c18,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x25c,
        size: 0x1048,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x264,
        size: 0x0e10,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x481,
        size: 0x12688,
        role_only: Some(FirmwareRole::Gfx1),
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x26c,
        size: 0x0068,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x274,
        size: 0x0800,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x010,
        size: 0x0088,
        role_only: None,
    },
];

fn address_representable(address: u64) -> bool {
    let high = address & !INITDATA_ADDR_MASK;
    high == 0 || high == INITDATA_ADDR_PREFIX
}

fn required_address(address: u64) -> Result<(), InitdataError> {
    if address == 0 {
        Err(InitdataError::NullRequiredAddress)
    } else if !address_representable(address) {
        Err(InitdataError::AddressOutOfRange)
    } else {
        Ok(())
    }
}

fn optional_address(address: u64) -> Result<(), InitdataError> {
    if address == 0 || address_representable(address) {
        Ok(())
    } else {
        Err(InitdataError::AddressOutOfRange)
    }
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

/// Validate the cross-role invariants established before either ASC is kicked.
pub(crate) fn validate_pair(addresses: &[RoleAddresses; 2]) -> Result<(), InitdataError> {
    if addresses[0].shared != addresses[1].shared {
        return Err(InitdataError::PairSharedPointerMismatch);
    }
    if addresses[0].root == addresses[1].root {
        return Err(InitdataError::PairAliasedRoot);
    }
    if addresses[0].runtime_pointers == addresses[1].runtime_pointers {
        return Err(InitdataError::PairAliasedRuntimePointers);
    }
    Ok(())
}

/// Encode the grounded subset for one G17 firmware role.
///
/// Every output is zeroed first. This makes unknown bytes deterministic and,
/// critically, prevents stale host memory from becoming invented firmware ABI.
pub(crate) fn encode_grounded_subset(
    role: FirmwareRole,
    addresses: &RoleAddresses,
    perf_state_clock_mhz: &[u32],
    root: &mut [u8],
    runtime_pointers: &mut [u8],
    hw_globals: &mut [u8],
) -> Result<(), InitdataError> {
    if root.len() != ROOT_SIZE
        || runtime_pointers.len() != RUNTIME_POINTERS_SIZE
        || hw_globals.len() != HW_GLOBALS_SIZE
    {
        return Err(InitdataError::BufferSize);
    }
    if perf_state_clock_mhz.is_empty() || perf_state_clock_mhz.len() > PERF_STATE_MAX {
        return Err(InitdataError::PerfStateCount);
    }

    for address in [
        addresses.root,
        addresses.runtime_pointers,
        addresses.hw_globals,
        addresses.shared,
        addresses.role,
        addresses.cross_role,
    ] {
        required_address(address)?;
    }
    optional_address(addresses.firmware_shared_allocation)?;

    match role {
        FirmwareRole::Gfx if addresses.cross_role_2 != 0 => {
            return Err(InitdataError::RoleSpecificAddress);
        }
        FirmwareRole::Gfx1 => required_address(addresses.cross_role_2)?,
        FirmwareRole::Gfx => {}
    }

    root.fill(0);
    runtime_pointers.fill(0);
    hw_globals.fill(0);

    put_u64(root, 0x00, INITDATA_MAGIC);
    put_u64(
        root,
        ROOT_FW_SHARED_ALLOC,
        addresses.firmware_shared_allocation,
    );
    put_u64(root, ROOT_RUNTIME_POINTERS, addresses.runtime_pointers);
    put_u64(root, ROOT_SHARED, addresses.shared);
    put_u32(root, ROOT_ROLE, role as u32);
    put_u32(root, ROOT_HOST_MAPPED_ALLOCS, 1);
    put_u64(root, ROOT_ROLE_POINTER, addresses.role);
    match role {
        FirmwareRole::Gfx => put_u64(root, ROOT_ROLE0_CROSS, addresses.cross_role),
        FirmwareRole::Gfx1 => {
            put_u64(root, ROOT_ROLE1_CROSS, addresses.cross_role);
            put_u64(root, ROOT_ROLE1_CROSS_2, addresses.cross_role_2);
        }
    }

    put_u64(runtime_pointers, RT_HW_GLOBALS, addresses.hw_globals);
    put_u64(runtime_pointers, RT_ROLE_POINTER, addresses.role);

    put_u32(
        hw_globals,
        HW_PERF_STATE0_CLOCK_MHZ,
        perf_state_clock_mhz[0],
    );
    for (index, clock) in perf_state_clock_mhz.iter().copied().enumerate() {
        put_u32(hw_globals, HW_PERF_STATE_CLOCK_MHZ + index * 4, clock);
    }
    put_u32(
        hw_globals,
        HW_PERF_STATE_COUNT_MINUS_ONE,
        perf_state_clock_mhz.len() as u32 - 1,
    );

    Ok(())
}

/// Reproduce the two firmware acceptance checks made while parsing a root.
/// Passing this does not mean the rest of init-data is complete.
pub(crate) fn check_firmware_root_gates(root: &[u8]) -> Result<(), InitdataError> {
    if root.len() != ROOT_SIZE {
        return Err(InitdataError::BufferSize);
    }
    if get_u64(root, 0x00) != INITDATA_MAGIC {
        return Err(InitdataError::InterfaceMismatch);
    }
    if get_u32(root, ROOT_HOST_MAPPED_ALLOCS) == 0 {
        return Err(InitdataError::HostMappedAllocationsDisabled);
    }
    Ok(())
}

/// Return the byte range of a proved channel descriptor.
///
/// Index 12 is refused because it would overlap the cursor at `+0x1a0`.
pub(crate) fn channel_descriptor_range(index: usize) -> Option<core::ops::Range<usize>> {
    if index >= RT_CHANNEL_DESCRIPTOR_COUNT {
        return None;
    }
    let start = RT_CHANNEL_DESCRIPTORS + index * RT_CHANNEL_DESCRIPTOR_STRIDE;
    Some(start..start + RT_CHANNEL_DESCRIPTOR_STRIDE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addresses(base: u64) -> RoleAddresses {
        RoleAddresses {
            root: base,
            runtime_pointers: base + 0x4000,
            hw_globals: base + 0x8000,
            firmware_shared_allocation: 0x5000_0000,
            shared: 0x6000_0000,
            role: base + 0xc000,
            cross_role: base + 0x1_0000,
            cross_role_2: 0,
        }
    }

    fn encode(role: FirmwareRole, addresses: &RoleAddresses) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut root = vec![0xa5; ROOT_SIZE];
        let mut runtime = vec![0xa5; RUNTIME_POINTERS_SIZE];
        let mut hw = vec![0xa5; HW_GLOBALS_SIZE];
        encode_grounded_subset(
            role,
            addresses,
            &[400, 500, 600, 700],
            &mut root,
            &mut runtime,
            &mut hw,
        )
        .unwrap();
        (root, runtime, hw)
    }

    #[test]
    fn allocation_graph_and_descriptor_boundary_are_exact() {
        assert_eq!(FIRMWARE_ALLOCATIONS[0].size, 0x2828);
        assert_eq!(FIRMWARE_ALLOCATIONS[4].runtime_pointer_offset, 0x481);
        assert_eq!(FIRMWARE_ALLOCATIONS[4].size, 0x12688);
        assert_eq!(FIRMWARE_ALLOCATIONS[4].role_only, Some(FirmwareRole::Gfx1));
        assert_eq!(channel_descriptor_range(0), Some(0x20..0x40));
        assert_eq!(channel_descriptor_range(11), Some(0x180..0x1a0));
        assert_eq!(channel_descriptor_range(12), None);
    }

    #[test]
    fn gfx_subset_encodes_the_proven_pointer_chain_and_perf_table() {
        let address = addresses(0x1000_0000);
        let (root, runtime, hw) = encode(FirmwareRole::Gfx, &address);

        assert_eq!(root.len(), 0xc8);
        assert_eq!(runtime.len(), 0x4c0);
        assert_eq!(hw.len(), 0x2828);
        assert_eq!(get_u64(&root, 0), INITDATA_MAGIC);
        assert_eq!(get_u64(&root, 0x18), address.runtime_pointers);
        assert_eq!(get_u32(&root, 0x28), 0);
        assert_eq!(get_u32(&root, 0x2c), 1);
        assert_eq!(get_u64(&root, 0xb0), address.cross_role);
        assert_eq!(get_u64(&root, 0xb8), 0);
        assert_eq!(get_u64(&runtime, 0), address.hw_globals);
        assert_eq!(get_u64(&runtime, 0x200), address.role);
        assert_eq!(get_u32(&hw, 0x10b8), 400);
        assert_eq!(get_u32(&hw, 0x10bc + 12), 700);
        assert_eq!(get_u32(&hw, 0x1b3c), 3);
        assert!(root[ROOT_GPU_CONFIG].iter().all(|byte| *byte == 0));
        assert_eq!(runtime[0x250], 0);
        assert_eq!(hw[0x2700], 0);
        assert!(check_firmware_root_gates(&root).is_ok());
    }

    #[test]
    fn gfx1_uses_only_its_proven_cross_role_fields() {
        let mut address = addresses(0x2000_0000);
        address.cross_role_2 = 0x7000_0000;
        let (root, _, _) = encode(FirmwareRole::Gfx1, &address);

        assert_eq!(get_u32(&root, 0x28), 1);
        assert_eq!(get_u64(&root, 0xb0), 0);
        assert_eq!(get_u64(&root, 0xb8), address.cross_role);
        assert_eq!(get_u64(&root, 0xc0), address.cross_role_2);
    }

    #[test]
    fn invalid_addresses_and_role_specific_fields_fail_closed() {
        let mut address = addresses(0x1000_0000);
        let mut root = vec![0; ROOT_SIZE];
        let mut runtime = vec![0; RUNTIME_POINTERS_SIZE];
        let mut hw = vec![0; HW_GLOBALS_SIZE];

        address.root = 1 << 44;
        assert_eq!(
            encode_grounded_subset(
                FirmwareRole::Gfx,
                &address,
                &[400],
                &mut root,
                &mut runtime,
                &mut hw,
            ),
            Err(InitdataError::AddressOutOfRange)
        );

        address = addresses(0x1000_0000);
        address.cross_role_2 = 0x7000_0000;
        assert_eq!(
            encode_grounded_subset(
                FirmwareRole::Gfx,
                &address,
                &[400],
                &mut root,
                &mut runtime,
                &mut hw,
            ),
            Err(InitdataError::RoleSpecificAddress)
        );
    }

    #[test]
    fn pair_requires_one_shared_object_and_distinct_role_roots() {
        let first = addresses(0x1000_0000);
        let mut second = addresses(0x2000_0000);
        second.cross_role_2 = 0x7000_0000;
        assert!(validate_pair(&[first, second]).is_ok());

        second.shared += 0x4000;
        assert_eq!(
            validate_pair(&[first, second]),
            Err(InitdataError::PairSharedPointerMismatch)
        );
        second.shared = first.shared;
        second.root = first.root;
        assert_eq!(
            validate_pair(&[first, second]),
            Err(InitdataError::PairAliasedRoot)
        );
    }

    #[test]
    fn root_gate_errors_are_distinct() {
        let address = addresses(0x1000_0000);
        let (mut root, _, _) = encode(FirmwareRole::Gfx, &address);

        root[0] ^= 1;
        assert_eq!(
            check_firmware_root_gates(&root),
            Err(InitdataError::InterfaceMismatch)
        );
        put_u64(&mut root, 0, INITDATA_MAGIC);
        put_u32(&mut root, ROOT_HOST_MAPPED_ALLOCS, 0);
        assert_eq!(
            check_firmware_root_gates(&root),
            Err(InitdataError::HostMappedAllocationsDisabled)
        );
    }
}

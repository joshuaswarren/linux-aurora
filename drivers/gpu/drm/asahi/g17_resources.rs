// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! T8140 shared UAT and per-role firmware resources.
//!
//! GFX and GFX1 use one 42-bit UAT. Their roots share the same TTBAT owner,
//! while their main, state, and status addresses remain role-specific. The
//! layout here follows the source-built m1n1 G17P graph. It does not start an
//! ASC or publish an init-data doorbell.

#[cfg(not(test))]
use kernel::prelude::*;

#[cfg(not(test))]
use crate::g17_initdata;
#[cfg(not(test))]
use crate::{driver::AsahiDevice, gem, mmu};
#[cfg(test)]
#[path = "g17_initdata.rs"]
mod g17_initdata;

use g17_initdata::{InstanceAddresses, InstanceRole, SharedAddresses};

/// The T8140 absent-handoff UAT constructor is available to production code.
pub(crate) const EXECUTABLE_T8140_SHARED_UAT_AVAILABLE: bool = true;
/// Production code can allocate and map the source-backed per-role set.
pub(crate) const EXECUTABLE_T8140_ROLE_RESOURCES_AVAILABLE: bool = true;

#[cfg(not(test))]
const ROOT_PAIR_ALLOC_SIZE: usize = 0xc000;
const PRIMARY_STATUS_A_STATE_GRID_OFFSET: u64 = 0xee40;
const SECONDARY_EXTRA_0_BEFORE_PRIMARY_STATUS_A: u64 = 0x6c0;
#[cfg(not(test))]
const STATUS_A_ALLOC_SIZE: usize = 0xb4000;
#[cfg(not(test))]
const PRIMARY_STATE_ALLOC_SIZE: usize = 0xc4000;
#[cfg(not(test))]
const SECONDARY_STATE_ALLOC_SIZE: usize = 0x4000;

/// One role's view of the common UAT owner and its private objects.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct RoleResourceBinding {
    pub(crate) role: InstanceRole,
    /// Physical base of the one TTBAT used by both roles.
    pub(crate) uat_owner: u64,
    pub(crate) instance: InstanceAddresses,
    pub(crate) state_grid: u64,
}

/// Addresses passed from the resource owner to the G17 init-data builder.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17ResourceHandoff {
    pub(crate) shared: SharedAddresses,
    pub(crate) primary: RoleResourceBinding,
    pub(crate) secondary: RoleResourceBinding,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ResourceError {
    NullUatOwner,
    SplitUatOwner,
    WrongRole,
    SharedLayout,
    RoleLayout,
    InitdataLayout,
}

/// Validate the exact ownership boundary before either root can be published.
pub(crate) fn validate_handoff(
    handoff: &G17ResourceHandoff,
) -> core::result::Result<(), ResourceError> {
    let primary = &handoff.primary;
    let secondary = &handoff.secondary;

    if primary.uat_owner == 0 || secondary.uat_owner == 0 {
        return Err(ResourceError::NullUatOwner);
    }
    if primary.uat_owner != secondary.uat_owner {
        return Err(ResourceError::SplitUatOwner);
    }
    if primary.role != InstanceRole::Primary || secondary.role != InstanceRole::Secondary {
        return Err(ResourceError::WrongRole);
    }

    let primary_main = handoff
        .shared
        .hw_data_bundle
        .checked_add(g17_initdata::NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET as u64)
        .ok_or(ResourceError::SharedLayout)?;
    let secondary_main = handoff
        .shared
        .hw_data_bundle
        .checked_add(g17_initdata::NATIVE_SECONDARY_MAIN_BUNDLE_OFFSET as u64)
        .ok_or(ResourceError::SharedLayout)?;
    if handoff.shared.hw_data_bundle_alloc != g17_initdata::NATIVE_SHARED_CLUSTER_SIZE
        || primary.instance.main_config != primary_main
        || secondary.instance.main_config != secondary_main
    {
        return Err(ResourceError::SharedLayout);
    }

    if primary.instance.status_b
        != primary
            .state_grid
            .checked_add(g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64)
            .ok_or(ResourceError::RoleLayout)?
        || primary.instance.status_a
            != primary
                .state_grid
                .checked_add(PRIMARY_STATUS_A_STATE_GRID_OFFSET)
                .ok_or(ResourceError::RoleLayout)?
        || secondary.instance.status_b != 0
        || primary.state_grid == secondary.state_grid
        || primary.instance.status_a == secondary.instance.status_a
    {
        return Err(ResourceError::RoleLayout);
    }

    let secondary_extras = [
        primary
            .instance
            .status_a
            .checked_sub(SECONDARY_EXTRA_0_BEFORE_PRIMARY_STATUS_A)
            .ok_or(ResourceError::RoleLayout)?,
        secondary
            .state_grid
            .checked_add(g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64)
            .ok_or(ResourceError::RoleLayout)?,
    ];
    if primary.instance.secondary_extras != [0, 0]
        || secondary.instance.secondary_extras != secondary_extras
    {
        return Err(ResourceError::RoleLayout);
    }

    g17_initdata::validate_pair(&primary.instance, &secondary.instance)
        .map_err(|_| ResourceError::InitdataLayout)?;

    let mut primary_root = [0u8; g17_initdata::ROOT_SIZE_PRIMARY];
    g17_initdata::encode_root(
        InstanceRole::Primary,
        &handoff.shared,
        &primary.instance,
        &mut primary_root,
    )
    .map_err(|_| ResourceError::InitdataLayout)?;
    let mut secondary_root = [0u8; g17_initdata::ROOT_SIZE_SECONDARY];
    g17_initdata::encode_root(
        InstanceRole::Secondary,
        &handoff.shared,
        &secondary.instance,
        &mut secondary_root,
    )
    .map_err(|_| ResourceError::InitdataLayout)?;

    Ok(())
}

#[cfg(not(test))]
struct MappedObject {
    mapping: mmu::KernelMapping,
    object: gem::ObjectRef,
    logical_size: usize,
}

#[cfg(not(test))]
impl MappedObject {
    fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        size: usize,
        alignment: u64,
        prot: mmu::Prot,
    ) -> Result<Self> {
        let mut object = gem::new_kernel_object(dev, size)?;
        object.vmap()?.memset(0);
        let mapping = object.map_into_range(
            uat.kernel_vm(),
            uat.kernel_va_range()?,
            alignment,
            prot,
            true,
        )?;
        Ok(Self {
            mapping,
            object,
            logical_size: size,
        })
    }

    fn iova(&self) -> u64 {
        self.mapping.iova()
    }

    fn write_roots(
        &mut self,
        shared: &SharedAddresses,
        primary: &InstanceAddresses,
        secondary: &InstanceAddresses,
    ) -> Result {
        let vmap = self.object.vmap()?;
        let bytes = unsafe {
            // SAFETY: the VMap covers this object's complete, page-rounded
            // allocation and logical_size never exceeds the object size.
            core::slice::from_raw_parts_mut(vmap.as_mut_ptr(), self.logical_size)
        };
        g17_initdata::encode_root(
            InstanceRole::Primary,
            shared,
            primary,
            &mut bytes[..g17_initdata::ROOT_SIZE_PRIMARY],
        )
        .map_err(|_| EINVAL)?;
        let secondary_offset = g17_initdata::SECONDARY_ROOT_DELTA as usize;
        g17_initdata::encode_root(
            InstanceRole::Secondary,
            shared,
            secondary,
            &mut bytes[secondary_offset..secondary_offset + g17_initdata::ROOT_SIZE_SECONDARY],
        )
        .map_err(|_| EINVAL)
    }
}

#[cfg(not(test))]
#[allow(dead_code)]
struct G17MappedResources {
    roots: MappedObject,
    shared_cluster: MappedObject,
    region_a: MappedObject,
    region_c: MappedObject,
    primary_state: MappedObject,
    secondary_state: MappedObject,
    secondary_status_a: MappedObject,
}

#[cfg(not(test))]
struct G17UatOwner {
    uat: KBox<mmu::Uat>,
}

/// One production owner for the T8140 UAT and every GFX/GFX1 mapping.
///
/// Role records contain addresses only. They cannot allocate a second UAT or
/// map private objects behind the owner's back.
#[cfg(not(test))]
#[allow(dead_code)]
pub(crate) struct G17ResourceOwner {
    handoff: G17ResourceHandoff,
    resources: G17MappedResources,
    address_space: G17UatOwner,
}

#[cfg(not(test))]
#[allow(dead_code)]
impl G17ResourceOwner {
    pub(crate) fn new(dev: &AsahiDevice) -> Result<Self> {
        let uat = KBox::new(mmu::Uat::new_t8140(dev, true)?, GFP_KERNEL)?;
        let address_space = G17UatOwner { uat };

        let mut roots = MappedObject::new(
            dev,
            &address_space.uat,
            ROOT_PAIR_ALLOC_SIZE,
            g17_initdata::SECONDARY_ROOT_DELTA,
            mmu::PROT_FW_SHARED_RO,
        )?;
        let shared_cluster = MappedObject::new(
            dev,
            &address_space.uat,
            g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_PRIV_RW,
        )?;
        let region_a = MappedObject::new(
            dev,
            &address_space.uat,
            g17_initdata::REGION_A_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RO,
        )?;
        let region_c = MappedObject::new(
            dev,
            &address_space.uat,
            g17_initdata::REGION_C_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let primary_state = MappedObject::new(
            dev,
            &address_space.uat,
            PRIMARY_STATE_ALLOC_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let secondary_state = MappedObject::new(
            dev,
            &address_space.uat,
            SECONDARY_STATE_ALLOC_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let secondary_status_a = MappedObject::new(
            dev,
            &address_space.uat,
            STATUS_A_ALLOC_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;

        let shared = SharedAddresses {
            region_a: region_a.iova(),
            region_c: region_c.iova(),
            hw_data_bundle: shared_cluster.iova(),
            hw_data_bundle_alloc: g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
        };
        let primary_instance = InstanceAddresses {
            root: roots.iova(),
            main_config: shared_cluster.iova()
                + g17_initdata::NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET as u64,
            status_a: primary_state.iova() + PRIMARY_STATUS_A_STATE_GRID_OFFSET,
            status_b: primary_state.iova()
                + g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64,
            secondary_extras: [0, 0],
        };
        let secondary_instance = InstanceAddresses {
            root: roots.iova() + g17_initdata::SECONDARY_ROOT_DELTA,
            main_config: shared_cluster.iova()
                + g17_initdata::NATIVE_SECONDARY_MAIN_BUNDLE_OFFSET as u64,
            status_a: secondary_status_a.iova(),
            status_b: 0,
            secondary_extras: [
                primary_instance.status_a - SECONDARY_EXTRA_0_BEFORE_PRIMARY_STATUS_A,
                secondary_state.iova() + g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64,
            ],
        };
        let uat_owner = address_space.uat.ttb_base();
        let handoff = G17ResourceHandoff {
            shared,
            primary: RoleResourceBinding {
                role: InstanceRole::Primary,
                uat_owner,
                instance: primary_instance,
                state_grid: primary_state.iova(),
            },
            secondary: RoleResourceBinding {
                role: InstanceRole::Secondary,
                uat_owner,
                instance: secondary_instance,
                state_grid: secondary_state.iova(),
            },
        };
        validate_handoff(&handoff).map_err(|_| EINVAL)?;
        roots.write_roots(&shared, &primary_instance, &secondary_instance)?;

        Ok(Self {
            handoff,
            resources: G17MappedResources {
                roots,
                shared_cluster,
                region_a,
                region_c,
                primary_state,
                secondary_state,
                secondary_status_a,
            },
            address_space,
        })
    }

    pub(crate) fn handoff(&self) -> &G17ResourceHandoff {
        &self.handoff
    }

    pub(crate) fn uat(&self) -> &mmu::Uat {
        &self.address_space.uat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KERN: u64 = 0xffff_fc20_0000_0000;

    fn valid_handoff() -> G17ResourceHandoff {
        let bundle = KERN + 0x4000_0000;
        let primary_state = KERN + 0x1000_0000;
        let secondary_state = KERN + 0x2000_0000;
        let primary_status_a = primary_state + PRIMARY_STATUS_A_STATE_GRID_OFFSET;
        G17ResourceHandoff {
            shared: SharedAddresses {
                region_a: KERN + 0x0100_0000,
                region_c: KERN + 0x0100_4000,
                hw_data_bundle: bundle,
                hw_data_bundle_alloc: g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
            },
            primary: RoleResourceBinding {
                role: InstanceRole::Primary,
                uat_owner: 0x9000_0000,
                instance: InstanceAddresses {
                    root: KERN + 0x0200_0000,
                    main_config: bundle + g17_initdata::NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET as u64,
                    status_a: primary_status_a,
                    status_b: primary_state
                        + g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64,
                    secondary_extras: [0, 0],
                },
                state_grid: primary_state,
            },
            secondary: RoleResourceBinding {
                role: InstanceRole::Secondary,
                uat_owner: 0x9000_0000,
                instance: InstanceAddresses {
                    root: KERN + 0x0200_0000 + g17_initdata::SECONDARY_ROOT_DELTA,
                    main_config: bundle + g17_initdata::NATIVE_SECONDARY_MAIN_BUNDLE_OFFSET as u64,
                    status_a: KERN + 0x3000_0000,
                    status_b: 0,
                    secondary_extras: [
                        primary_status_a - SECONDARY_EXTRA_0_BEFORE_PRIMARY_STATUS_A,
                        secondary_state + g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64,
                    ],
                },
                state_grid: secondary_state,
            },
        }
    }

    #[test]
    fn exact_t8140_handoff_is_executable_and_source_accepted() {
        assert!(EXECUTABLE_T8140_SHARED_UAT_AVAILABLE);
        assert!(EXECUTABLE_T8140_ROLE_RESOURCES_AVAILABLE);
        assert_eq!(validate_handoff(&valid_handoff()), Ok(()));
    }

    #[test]
    fn split_or_null_uat_owner_is_invalid() {
        let mut handoff = valid_handoff();
        handoff.secondary.uat_owner += 0x4000;
        assert_eq!(
            validate_handoff(&handoff),
            Err(ResourceError::SplitUatOwner)
        );

        let mut handoff = valid_handoff();
        handoff.primary.uat_owner = 0;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::NullUatOwner));
    }

    #[test]
    fn cross_role_or_aliased_resources_are_invalid() {
        let mut handoff = valid_handoff();
        handoff.primary.role = InstanceRole::Secondary;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::WrongRole));

        let mut handoff = valid_handoff();
        handoff.secondary.instance.main_config = handoff.primary.instance.main_config;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::SharedLayout));

        let mut handoff = valid_handoff();
        handoff.secondary.state_grid = handoff.primary.state_grid;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::RoleLayout));
    }

    #[test]
    fn role_specific_status_and_extra_relations_are_invalid() {
        let mut handoff = valid_handoff();
        handoff.secondary.instance.status_b = KERN + 0x4000;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::RoleLayout));

        let mut handoff = valid_handoff();
        handoff.secondary.instance.secondary_extras[1] += 0x4000;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::RoleLayout));

        let mut handoff = valid_handoff();
        handoff.primary.instance.secondary_extras[0] = KERN + 0x8000;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::RoleLayout));
    }
}

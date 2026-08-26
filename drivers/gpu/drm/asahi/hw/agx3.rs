// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! AGX3 (G15/G16/G17) SoC identification configuration.
//!
//! Unlike the AGX2 [`super::HwConfig`] instances, these entries carry *only*
//! the identification and UAT-geometry facts recovered from the pinned
//! `26A5378n` kext corpus and the live T6050 machine:
//!
//! - chip id and `apple,agx-*` matching,
//! - the hardware family/variant bytes reported at `0xd04000` and the die
//!   count that disambiguates the C/D split,
//! - the decoded design generation and variant (`readChipInfo` decode table,
//!   `GPU_DRIVER_CONTRACT_G15_G17_2026-08-23.md` section 2.4/2.5),
//! - the UAT geometry: 42-bit input roots (six-bit L1 index) and a 42-bit
//!   output address space (`isPhysicalAddressValid` gate), constant across
//!   every AGX3 part.
//!
//! Everything that is per-firmware-ABI — `HwConfigA`/`HwConfigB` tuning
//! blocks, shared tables, IO mappings, preemption buffer sizes, the
//! firmware `GpuCore` id (renumbered per firmware build) — is deliberately
//! absent. No value here permits a firmware handoff; the probe path fails
//! closed before DMA setup or any ASC start for these targets.

#[cfg(not(test))]
use crate::identity::{GpuGen, GpuVariant};
#[cfg(test)]
#[path = "../identity.rs"]
mod identity;
#[cfg(test)]
use identity::{GpuGen, GpuVariant};

/// Identification-only configuration for one AGX3 SoC.
#[derive(Debug)]
pub(crate) struct SocConfig {
    /// Chip ID in hex format (e.g. 0x6050 for t6050).
    pub(crate) chip_id: u32,
    /// Decoded design generation for this chip.
    pub(crate) gpu_gen: GpuGen,
    /// Decoded variant for this chip.
    pub(crate) gpu_variant: GpuVariant,
    /// Hardware family byte expected at `0xd04000[31:24]`.
    pub(crate) hw_family: u8,
    /// Hardware variant byte expected at `0xd04000[23:16]`.
    pub(crate) hw_variant: u8,
    /// Die count expected at `0xd04010[19:16]`.
    pub(crate) num_dies: u32,
    /// Input-address bits per UAT root (42 on every AGX3 part).
    pub(crate) uat_ias: u8,
    /// Output address space (42-bit on every AGX3 part).
    pub(crate) uat_oas: u32,
}

const fn agx3(
    chip_id: u32,
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
    hw_family: u8,
    hw_variant: u8,
    num_dies: u32,
) -> SocConfig {
    SocConfig {
        chip_id,
        gpu_gen,
        gpu_variant,
        hw_family,
        hw_variant,
        num_dies,
        uat_ias: 42,
        uat_oas: 42,
    }
}

// G15 (M3 family). Family byte 0x7; decode read at AGXG15G `readChipInfo`
// 0xfffffe0008af2120/0xfffffe0008af21c0. Variant 4 splits C/D on die count.
/// Apple M3 (G15G).
pub(crate) const T8122: SocConfig = agx3(0x8122, GpuGen::G15, GpuVariant::G, 0x7, 2, 1);
/// Apple M3 Pro (G15S).
pub(crate) const T6030: SocConfig = agx3(0x6030, GpuGen::G15, GpuVariant::S, 0x7, 3, 1);
/// Apple M3 Max (G15C).
pub(crate) const T6031: SocConfig = agx3(0x6031, GpuGen::G15, GpuVariant::C, 0x7, 4, 1);
/// Apple M3 Ultra (G15D, two dies).
pub(crate) const T6032: SocConfig = agx3(0x6032, GpuGen::G15, GpuVariant::D, 0x7, 4, 2);

// G16 (M4 family) and A18 Pro. Family byte 0xa is shared with G17P/G17A;
// the variant byte discriminates. Decode read at AGXG16G
// 0xfffffe0008b5abd8 et seq. The T8140/G17P identity and D93AP topology are
// independently pinned in agx_power_recovery.rs; this entry permits only the
// existing read-only diagnostic probe and never a firmware handoff.
/// Apple A18 Pro (G17P).
pub(crate) const T8140: SocConfig = agx3(0x8140, GpuGen::G17, GpuVariant::P, 0xa, 0, 1);
/// Apple M4 (G16G).
pub(crate) const T8132: SocConfig = agx3(0x8132, GpuGen::G16, GpuVariant::G, 0xa, 2, 1);
/// Apple M4 Pro (G16S).
pub(crate) const T6040: SocConfig = agx3(0x6040, GpuGen::G16, GpuVariant::S, 0xa, 3, 1);
/// Apple M4 Max (G16C).
pub(crate) const T6041: SocConfig = agx3(0x6041, GpuGen::G16, GpuVariant::C, 0xa, 4, 1);

// G17 (M5 family). Family byte 0xb. Decode read at AGXG17X
// 0xfffffe0008b4b0a8/0xfffffe0008b4b524/0xfffffe0008b4b538. T6050 is
// additionally verified live (Mac17,9: gpu_gen=17, gpu_var="S", 2 clusters).
/// Apple M5 (G17G).
pub(crate) const T8142: SocConfig = agx3(0x8142, GpuGen::G17, GpuVariant::G, 0xb, 2, 1);
/// Apple M5 Pro (G17S).
pub(crate) const T6050: SocConfig = agx3(0x6050, GpuGen::G17, GpuVariant::S, 0xb, 3, 1);
/// Apple M5 Max (G17C).
pub(crate) const T6051: SocConfig = agx3(0x6051, GpuGen::G17, GpuVariant::C, 0xb, 4, 1);

// t6034 and t6052 appear in the AGXG15C/AGXG17X kext match lists but map to
// no distinct core identity in the readChipInfo decode; what silicon they
// name is unknown, so they deliberately have no entry here.

#[cfg(test)]
mod tests {
    use super::identity::{
        decode_gpu_identity, FirmwareRoleTopology, GpuGen, GpuHalGeneration, SubmissionTransport,
    };
    use super::*;

    const ALL: [&SocConfig; 11] = [
        &T8122, &T6030, &T6031, &T6032, &T8140, &T8132, &T6040, &T6041, &T8142, &T6050, &T6051,
    ];

    #[test]
    fn every_soc_decodes_to_its_stated_identity() {
        for soc in ALL {
            let identity = decode_gpu_identity(soc.hw_family, soc.hw_variant, soc.num_dies as u8)
                .expect("static AGX3 identity must decode");
            assert_eq!(identity.gpu_gen, soc.gpu_gen, "chip {:#x}", soc.chip_id);
            assert_eq!(
                identity.gpu_variant, soc.gpu_variant,
                "chip {:#x}",
                soc.chip_id
            );
            // Every Mac AGX3 part is USC generation 3 with 42-bit UAT roots.
            assert_eq!(identity.usc_generation, 3, "chip {:#x}", soc.chip_id);
            assert_eq!(
                identity.uat_input_address_bits, soc.uat_ias,
                "chip {:#x}",
                soc.chip_id
            );
            assert_eq!(soc.uat_ias, 42);
            assert_eq!(soc.uat_oas, 42);
        }
    }

    #[test]
    fn firmware_role_topology_follows_the_generation() {
        for soc in ALL {
            let identity =
                decode_gpu_identity(soc.hw_family, soc.hw_variant, soc.num_dies as u8).unwrap();
            let expected = if soc.gpu_gen == GpuGen::G17 {
                FirmwareRoleTopology::Dual
            } else {
                FirmwareRoleTopology::Single
            };
            assert_eq!(identity.firmware_roles, expected, "chip {:#x}", soc.chip_id);
        }
    }

    #[test]
    fn hal300_parts_are_sksm_only() {
        for soc in ALL {
            let identity =
                decode_gpu_identity(soc.hw_family, soc.hw_variant, soc.num_dies as u8).unwrap();
            if identity.gpu_hal_generation == GpuHalGeneration::Hal300 {
                assert_eq!(
                    identity.submission_transport,
                    SubmissionTransport::Sksm,
                    "chip {:#x}",
                    soc.chip_id
                );
            }
        }
    }

    #[test]
    fn chip_ids_are_unique_and_match_their_family_tier() {
        for (i, a) in ALL.iter().enumerate() {
            for b in ALL.iter().skip(i + 1) {
                assert_ne!(a.chip_id, b.chip_id);
            }
            // Mac 0x8xxx chips are single-cluster consumer parts (G variant),
            // except mobile T8140 whose independently decoded tier is P.
            // 0x6xxx chips are the S/C/D tiers.
            if a.chip_id == 0x8140 {
                assert_eq!(a.gpu_variant, GpuVariant::P);
            } else if a.chip_id & 0xf000 == 0x8000 {
                assert_eq!(a.gpu_variant, GpuVariant::G);
            } else {
                assert_ne!(a.gpu_variant, GpuVariant::G);
            }
        }
    }

    #[test]
    fn t8140_is_the_exact_a18_pro_g17p_diagnostic_identity() {
        assert_eq!(T8140.chip_id, 0x8140);
        assert_eq!(T8140.hw_family, 0xa);
        assert_eq!(T8140.hw_variant, 0);
        assert_eq!(T8140.num_dies, 1);
        let identity =
            decode_gpu_identity(T8140.hw_family, T8140.hw_variant, T8140.num_dies as u8).unwrap();
        assert_eq!(identity.gpu_gen, GpuGen::G17);
        assert_eq!(identity.gpu_variant, GpuVariant::P);
    }
}

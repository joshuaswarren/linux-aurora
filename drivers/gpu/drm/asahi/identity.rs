// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! GPU identity decoding shared by register discovery and host-side tests.
//!
//! Apple's hardware family byte is not the GPU generation. In particular,
//! family 0xa contains both G16 and G17 parts, so all identity decisions must
//! include the hardware variant byte (and, for the older C/D split, die count).

/// GPU generation enumeration. Note: Part of the UABI.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[repr(u32)]
pub(crate) enum GpuGen {
    G13 = 13,
    G14 = 14,
    G15 = 15,
    G16 = 16,
    G17 = 17,
}

/// GPU variant enumeration. Note: Part of the UABI.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[repr(u32)]
pub(crate) enum GpuVariant {
    P = 'P' as u32,
    G = 'G' as u32,
    S = 'S' as u32,
    C = 'C' as u32,
    D = 'D' as u32,
    A = 'A' as u32,
}

/// Apple's GPU HAL generation.
///
/// G13 through G15 predate the numbered HAL namespace. This is separate from
/// both the design generation and the USC generation: G17P/G17A use HAL200,
/// while the other G17 parts use HAL300.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[repr(u32)]
pub(crate) enum GpuHalGeneration {
    Legacy = 0,
    Hal200 = 200,
    Hal300 = 300,
}

/// Firmware-role topology presented by Apple's RTBuddy driver.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum FirmwareRoleTopology {
    /// One GFX firmware role.
    Single,
    /// Separate GFX and GFX1 firmware roles.
    Dual,
}

/// Host-to-firmware submission transport present in Apple's driver.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum SubmissionTransport {
    /// Classic device-control ring and SKU channels only.
    Classic,
    /// Both classic and 128-queue SKSM stacks are linked.
    ClassicAndSksm,
    /// 128-queue SKSM transport only.
    Sksm,
}

/// Exact host-reported bit layout used to insert a queue ID in an SKSM kick.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct SksmKickQueueIdConfig {
    pub(crate) shift: u8,
    pub(crate) mask: u8,
}

/// Firmware role selected by Apple's host kick sender.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum SubmissionFirmwareRole {
    Gfx,
    Gfx1,
}

/// Exact role and wire word produced for one host firmware kick.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct FirmwareKickRoute {
    pub(crate) role: SubmissionFirmwareRole,
    pub(crate) message: u64,
}

/// Refusals made by the target-pinned SKSM submission preflight.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum SubmissionConfigError {
    /// No independent pinned artifact proves SKSM configuration for the target.
    UnsupportedTarget,
    /// The supplied configuration differs from the target's exact host values.
    UnexpectedSksmKickQueueIdConfig,
    /// The kick index does not fit the three-bit host field.
    KickIndexOutOfRange,
    /// The opaque firmware message type does not fit the six-bit host field.
    FirmwareMessageTypeOutOfRange,
}

/// A pure submission configuration validator is available for the exact
/// G16X/G17 host targets below.
#[allow(dead_code)]
pub(crate) const GROUNDED_SKSM_KICK_QUEUE_ID_CONFIG_AVAILABLE: bool = true;
/// Queue construction, transport selection on dual-stack targets, shared
/// memory ownership, and the complete firmware notification sequence are not
/// modeled, so command submission must remain disconnected from this helper.
#[allow(dead_code)]
pub(crate) const EXECUTABLE_G15_G17_SUBMISSION_AVAILABLE: bool = false;

const SKSM_KICK_QUEUE_ID_CONFIG: SksmKickQueueIdConfig = SksmKickQueueIdConfig {
    shift: 40,
    mask: 0x7f,
};
const FIRMWARE_KICK_TAG: u64 = 0x0080_0000_0000_0000;
const FIRMWARE_KICK_INDEX_MASK: u8 = 0x07;
const FIRMWARE_KICK_INDEX_SHIFT: u32 = 2;
const FIRMWARE_MESSAGE_TYPE_MASK: u8 = 0x3f;
const FIRMWARE_MESSAGE_TYPE_SHIFT: u32 = 48;
const G17_GFX1_FIRMWARE_MESSAGE_TYPE: u8 = 0x07;

/// Identity and userspace-visible architecture axes decoded from GPU ID MMIO.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct GpuIdentity {
    pub(crate) gpu_gen: GpuGen,
    pub(crate) gpu_variant: GpuVariant,
    pub(crate) usc_generation: u32,
    pub(crate) gpu_hal_generation: GpuHalGeneration,
    pub(crate) firmware_roles: FirmwareRoleTopology,
    pub(crate) submission_transport: SubmissionTransport,
    /// Input-address bits per UAT root.
    pub(crate) uat_input_address_bits: u8,
}

/// Decode the hardware family/variant/die tuple reported at 0xd04000/0xd04010.
///
/// This deliberately does not imply that firmware, initdata, command
/// submission, or a core-mask register layout is implemented for the returned
/// identity.
pub(crate) fn decode_gpu_identity(family: u8, variant: u8, num_dies: u8) -> Option<GpuIdentity> {
    use GpuGen::*;
    use GpuHalGeneration::*;
    use GpuVariant::*;

    let (gpu_gen, gpu_variant, usc_generation, gpu_hal_generation) = match (family, variant) {
        (0x4, 0) => (G13, P, 2, Legacy),
        (0x4, 2) => (G13, G, 2, Legacy),
        (0x4, 3) => (G13, S, 2, Legacy),
        // G13 has no D core identity; C covers both Max and Ultra.
        (0x4, 4) => (G13, C, 2, Legacy),

        (0x5, 0) => (G14, P, 2, Legacy),
        (0x5, 2) => (G14, G, 2, Legacy),
        (0x6, 3) => (G14, S, 2, Legacy),
        (0x6, 4) => match num_dies {
            1 => (G14, C, 2, Legacy),
            2 => (G14, D, 2, Legacy),
            _ => return None,
        },

        // G15P is A16, not an M3 GPU. Apple's compiler routes
        // applegpu_g15p through the AGX2 backend even though the design
        // generation is 15. Keep USC generation independent from gpu_gen.
        (0x7, 0) => (G15, P, 2, Legacy),
        (0x7, 2) => (G15, G, 3, Legacy),
        (0x7, 3) => (G15, S, 3, Legacy),
        (0x7, 4) => match num_dies {
            1 => (G15, C, 3, Legacy),
            2 => (G15, D, 3, Legacy),
            _ => return None,
        },

        (0x8, 0) => (G16, P, 3, Hal200),

        // Family 0xa crosses the design-generation boundary.
        (0xa, 0) => (G17, P, 3, Hal200),
        (0xa, 2) => (G16, G, 3, Hal200),
        (0xa, 3) => (G16, S, 3, Hal200),
        (0xa, 4) => (G16, C, 3, Hal200),
        (0xa, 5) => (G17, A, 3, Hal200),

        (0xb, 2) => (G17, G, 3, Hal300),
        (0xb, 3) => (G17, S, 3, Hal300),
        (0xb, 4) => (G17, C, 3, Hal300),
        _ => return None,
    };

    let firmware_roles = if gpu_gen == G17 {
        FirmwareRoleTopology::Dual
    } else {
        FirmwareRoleTopology::Single
    };

    let submission_transport = match (family, variant) {
        // G16X and G17P/G17A carry both stacks. The runtime selector is not
        // yet known, so callers must not silently choose one.
        (0xa, 0 | 3 | 4 | 5) => SubmissionTransport::ClassicAndSksm,
        // HAL300 removes the classic SKU channel stack.
        (0xb, 2 | 3 | 4) => SubmissionTransport::Sksm,
        _ => SubmissionTransport::Classic,
    };

    let uat_input_address_bits = if matches!(gpu_gen, G13 | G14) { 39 } else { 42 };

    Some(GpuIdentity {
        gpu_gen,
        gpu_variant,
        usc_generation,
        gpu_hal_generation,
        firmware_roles,
        submission_transport,
        uat_input_address_bits,
    })
}

/// Return the independently recovered SKSM queue-ID layout for one target.
///
/// The values are byte-identical in the pinned G16X (G16S/C), G17P, G17G and
/// G17X (G17S/C) host drivers. G17A is deliberately absent: sharing a host
/// kext with G17P is not independent target evidence. Classic-only targets do
/// not have an SKSM layout, while the presence of a layout on G16X/G17P does
/// not resolve their still-unknown classic-versus-SKSM runtime selector.
#[allow(dead_code)]
pub(crate) const fn sksm_kick_queue_id_config(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
) -> Option<SksmKickQueueIdConfig> {
    use GpuGen::*;
    use GpuVariant::*;

    match (gpu_gen, gpu_variant) {
        (G16, S | C) | (G17, P | G | S | C) => Some(SKSM_KICK_QUEUE_ID_CONFIG),
        _ => None,
    }
}

/// Validate externally recovered SKSM queue-ID properties for one target.
#[allow(dead_code)]
pub(crate) const fn validate_sksm_kick_queue_id_config(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
    shift: u8,
    mask: u8,
) -> Result<SksmKickQueueIdConfig, SubmissionConfigError> {
    let Some(expected) = sksm_kick_queue_id_config(gpu_gen, gpu_variant) else {
        return Err(SubmissionConfigError::UnsupportedTarget);
    };

    if shift != expected.shift || mask != expected.mask {
        Err(SubmissionConfigError::UnexpectedSksmKickQueueIdConfig)
    } else {
        Ok(expected)
    }
}

/// Encode the exact cross-generation host firmware-kick word and role route.
///
/// Pinned G15/G16 hosts always send through their single `GFX` role. Every
/// pinned G17 host instead selects `GFX1` only for opaque message type seven;
/// all other six-bit message types use `GFX`. The destination endpoint and
/// the complete queue publication/notification lifecycle remain deliberately
/// outside this model.
#[allow(dead_code)]
pub(crate) const fn encode_firmware_kick_route(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
    kick_index: u8,
    message_type: u8,
) -> Result<FirmwareKickRoute, SubmissionConfigError> {
    use GpuGen::*;
    use GpuVariant::*;

    let target_is_proven = matches!(
        (gpu_gen, gpu_variant),
        (G15, G | S | C | D) | (G16, G | S | C) | (G17, P | G | S | C)
    );
    if !target_is_proven {
        return Err(SubmissionConfigError::UnsupportedTarget);
    }
    if kick_index > FIRMWARE_KICK_INDEX_MASK {
        return Err(SubmissionConfigError::KickIndexOutOfRange);
    }
    if message_type > FIRMWARE_MESSAGE_TYPE_MASK {
        return Err(SubmissionConfigError::FirmwareMessageTypeOutOfRange);
    }

    let role = match (gpu_gen, message_type) {
        (G17, G17_GFX1_FIRMWARE_MESSAGE_TYPE) => SubmissionFirmwareRole::Gfx1,
        _ => SubmissionFirmwareRole::Gfx,
    };
    let message = FIRMWARE_KICK_TAG
        | ((message_type as u64) << FIRMWARE_MESSAGE_TYPE_SHIFT)
        | ((kick_index as u64) << FIRMWARE_KICK_INDEX_SHIFT);

    Ok(FirmwareKickRoute { role, message })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(family: u8, variant: u8, dies: u8) -> GpuIdentity {
        decode_gpu_identity(family, variant, dies).expect("identity should decode")
    }

    #[test]
    fn variant_zero_is_p_and_variant_one_is_rejected() {
        assert_eq!(id(0x4, 0, 1).gpu_variant, GpuVariant::P);
        assert_eq!(id(0x7, 0, 1).gpu_variant, GpuVariant::P);
        assert_eq!(id(0xa, 0, 1).gpu_variant, GpuVariant::P);
        assert!(decode_gpu_identity(0x4, 1, 1).is_none());
        assert!(decode_gpu_identity(0x7, 1, 1).is_none());
    }

    #[test]
    fn family_a_crosses_the_generation_boundary() {
        assert_eq!(id(0xa, 0, 1).gpu_gen, GpuGen::G17);
        assert_eq!(id(0xa, 0, 1).gpu_variant, GpuVariant::P);
        assert_eq!(id(0xa, 2, 1).gpu_gen, GpuGen::G16);
        assert_eq!(id(0xa, 4, 1).gpu_variant, GpuVariant::C);
        assert_eq!(id(0xa, 5, 1).gpu_variant, GpuVariant::A);
    }

    #[test]
    fn c_d_split_uses_die_count_only_where_apple_does() {
        assert_eq!(id(0x6, 4, 1).gpu_variant, GpuVariant::C);
        assert_eq!(id(0x6, 4, 2).gpu_variant, GpuVariant::D);
        assert_eq!(id(0x7, 4, 1).gpu_variant, GpuVariant::C);
        assert_eq!(id(0x7, 4, 2).gpu_variant, GpuVariant::D);
        assert_eq!(id(0xa, 4, 2).gpu_variant, GpuVariant::C);
        assert_eq!(id(0xb, 4, 2).gpu_variant, GpuVariant::C);
    }

    #[test]
    fn architecture_axes_are_independent() {
        let g14 = id(0x6, 3, 1);
        assert_eq!(g14.usc_generation, 2);
        assert_eq!(g14.gpu_hal_generation, GpuHalGeneration::Legacy);

        let g15p = id(0x7, 0, 1);
        assert_eq!(g15p.gpu_gen, GpuGen::G15);
        assert_eq!(g15p.usc_generation, 2);

        let g15 = id(0x7, 2, 1);
        assert_eq!(g15.usc_generation, 3);
        assert_eq!(g15.gpu_hal_generation, GpuHalGeneration::Legacy);

        let g17p = id(0xa, 0, 1);
        assert_eq!(g17p.gpu_gen, GpuGen::G17);
        assert_eq!(g17p.gpu_hal_generation, GpuHalGeneration::Hal200);

        let g17g = id(0xb, 2, 1);
        assert_eq!(g17g.gpu_gen, GpuGen::G17);
        assert_eq!(g17g.gpu_hal_generation, GpuHalGeneration::Hal300);
    }

    #[test]
    fn g15_g16_g17_role_and_submission_routing_is_exact() {
        for (variant, dies) in [(0, 1), (2, 1), (3, 1), (4, 1), (4, 2)] {
            let g15 = id(0x7, variant, dies);
            assert_eq!(g15.gpu_gen, GpuGen::G15);
            assert_eq!(g15.usc_generation, if variant == 0 { 2 } else { 3 });
            assert_eq!(g15.gpu_hal_generation, GpuHalGeneration::Legacy);
            assert_eq!(g15.firmware_roles, FirmwareRoleTopology::Single);
            assert_eq!(g15.submission_transport, SubmissionTransport::Classic);
            assert_eq!(g15.uat_input_address_bits, 42);
        }

        for (family, variant, transport) in [
            (0x8, 0, SubmissionTransport::Classic),
            (0xa, 2, SubmissionTransport::Classic),
            (0xa, 3, SubmissionTransport::ClassicAndSksm),
            (0xa, 4, SubmissionTransport::ClassicAndSksm),
        ] {
            let g16 = id(family, variant, 1);
            assert_eq!(g16.gpu_gen, GpuGen::G16);
            assert_eq!(g16.gpu_hal_generation, GpuHalGeneration::Hal200);
            assert_eq!(g16.firmware_roles, FirmwareRoleTopology::Single);
            assert_eq!(g16.submission_transport, transport);
            assert_eq!(g16.uat_input_address_bits, 42);
        }

        for variant in [0, 5] {
            let g17_hal200 = id(0xa, variant, 1);
            assert_eq!(g17_hal200.gpu_gen, GpuGen::G17);
            assert_eq!(g17_hal200.gpu_hal_generation, GpuHalGeneration::Hal200);
            assert_eq!(g17_hal200.firmware_roles, FirmwareRoleTopology::Dual);
            assert_eq!(
                g17_hal200.submission_transport,
                SubmissionTransport::ClassicAndSksm
            );
        }

        for variant in [2, 3, 4] {
            let g17_hal300 = id(0xb, variant, 1);
            assert_eq!(g17_hal300.gpu_gen, GpuGen::G17);
            assert_eq!(g17_hal300.gpu_hal_generation, GpuHalGeneration::Hal300);
            assert_eq!(g17_hal300.firmware_roles, FirmwareRoleTopology::Dual);
            assert_eq!(g17_hal300.submission_transport, SubmissionTransport::Sksm);
            assert_eq!(g17_hal300.uat_input_address_bits, 42);
        }
    }

    #[test]
    fn sksm_kick_queue_id_config_is_exact_and_target_pinned() {
        for (gpu_gen, gpu_variant) in [
            (GpuGen::G16, GpuVariant::S),
            (GpuGen::G16, GpuVariant::C),
            (GpuGen::G17, GpuVariant::P),
            (GpuGen::G17, GpuVariant::G),
            (GpuGen::G17, GpuVariant::S),
            (GpuGen::G17, GpuVariant::C),
        ] {
            assert_eq!(
                validate_sksm_kick_queue_id_config(gpu_gen, gpu_variant, 40, 0x7f),
                Ok(SKSM_KICK_QUEUE_ID_CONFIG)
            );
            assert_eq!(
                validate_sksm_kick_queue_id_config(gpu_gen, gpu_variant, 39, 0x7f),
                Err(SubmissionConfigError::UnexpectedSksmKickQueueIdConfig)
            );
            assert_eq!(
                validate_sksm_kick_queue_id_config(gpu_gen, gpu_variant, 40, 0xff),
                Err(SubmissionConfigError::UnexpectedSksmKickQueueIdConfig)
            );
        }

        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::G),
            (GpuGen::G15, GpuVariant::S),
            (GpuGen::G15, GpuVariant::C),
            (GpuGen::G15, GpuVariant::D),
            (GpuGen::G16, GpuVariant::P),
            (GpuGen::G16, GpuVariant::G),
            (GpuGen::G17, GpuVariant::A),
        ] {
            assert_eq!(
                validate_sksm_kick_queue_id_config(gpu_gen, gpu_variant, 40, 0x7f),
                Err(SubmissionConfigError::UnsupportedTarget)
            );
        }

        assert!(GROUNDED_SKSM_KICK_QUEUE_ID_CONFIG_AVAILABLE);
        assert!(!EXECUTABLE_G15_G17_SUBMISSION_AVAILABLE);
    }

    #[test]
    fn firmware_kick_word_and_g17_role_route_are_exact() {
        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::G),
            (GpuGen::G15, GpuVariant::S),
            (GpuGen::G15, GpuVariant::C),
            (GpuGen::G15, GpuVariant::D),
            (GpuGen::G16, GpuVariant::G),
            (GpuGen::G16, GpuVariant::S),
            (GpuGen::G16, GpuVariant::C),
            (GpuGen::G17, GpuVariant::P),
            (GpuGen::G17, GpuVariant::G),
            (GpuGen::G17, GpuVariant::S),
            (GpuGen::G17, GpuVariant::C),
        ] {
            assert_eq!(
                encode_firmware_kick_route(gpu_gen, gpu_variant, 4, 3),
                Ok(FirmwareKickRoute {
                    role: SubmissionFirmwareRole::Gfx,
                    message: 0x0083_0000_0000_0010,
                })
            );
        }

        for gpu_variant in [
            GpuVariant::P,
            GpuVariant::G,
            GpuVariant::S,
            GpuVariant::C,
        ] {
            assert_eq!(
                encode_firmware_kick_route(GpuGen::G17, gpu_variant, 7, 7),
                Ok(FirmwareKickRoute {
                    role: SubmissionFirmwareRole::Gfx1,
                    message: 0x0087_0000_0000_001c,
                })
            );
        }
        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::G),
            (GpuGen::G16, GpuVariant::G),
        ] {
            assert_eq!(
                encode_firmware_kick_route(gpu_gen, gpu_variant, 7, 7),
                Ok(FirmwareKickRoute {
                    role: SubmissionFirmwareRole::Gfx,
                    message: 0x0087_0000_0000_001c,
                })
            );
        }

        assert_eq!(
            encode_firmware_kick_route(GpuGen::G17, GpuVariant::G, 8, 3),
            Err(SubmissionConfigError::KickIndexOutOfRange)
        );
        assert_eq!(
            encode_firmware_kick_route(GpuGen::G17, GpuVariant::G, 0, 0x40),
            Err(SubmissionConfigError::FirmwareMessageTypeOutOfRange)
        );
        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::P),
            (GpuGen::G16, GpuVariant::P),
            (GpuGen::G17, GpuVariant::A),
        ] {
            assert_eq!(
                encode_firmware_kick_route(gpu_gen, gpu_variant, 0, 3),
                Err(SubmissionConfigError::UnsupportedTarget)
            );
        }
    }

    #[test]
    fn a18_pro_g17p_acceptance_matrix() {
        let g17p = id(0xa, 0, 1);
        assert_eq!(
            (g17p.gpu_gen, g17p.gpu_variant),
            (GpuGen::G17, GpuVariant::P)
        );
        assert_eq!(g17p.gpu_hal_generation, GpuHalGeneration::Hal200);
        assert_eq!(g17p.firmware_roles, FirmwareRoleTopology::Dual);
        assert_eq!(
            g17p.submission_transport,
            SubmissionTransport::ClassicAndSksm
        );
        assert_eq!(g17p.uat_input_address_bits, 42);
    }

    #[test]
    fn m5_g17g_acceptance_matrix() {
        let g17g = id(0xb, 2, 1);
        assert_eq!(
            (g17g.gpu_gen, g17g.gpu_variant),
            (GpuGen::G17, GpuVariant::G)
        );
        assert_eq!(g17g.gpu_hal_generation, GpuHalGeneration::Hal300);
        assert_eq!(g17g.firmware_roles, FirmwareRoleTopology::Dual);
        assert_eq!(g17g.submission_transport, SubmissionTransport::Sksm);
        assert_eq!(g17g.uat_input_address_bits, 42);
    }

    #[test]
    fn m5_pro_g17s_acceptance_matrix() {
        let g17s = id(0xb, 3, 1);
        assert_eq!(
            (g17s.gpu_gen, g17s.gpu_variant),
            (GpuGen::G17, GpuVariant::S)
        );
        assert_eq!(g17s.gpu_hal_generation, GpuHalGeneration::Hal300);
        assert_eq!(g17s.firmware_roles, FirmwareRoleTopology::Dual);
        assert_eq!(g17s.submission_transport, SubmissionTransport::Sksm);
        assert_eq!(g17s.uat_input_address_bits, 42);
    }

    #[test]
    fn unknown_families_and_variants_are_rejected() {
        assert!(decode_gpu_identity(0x9, 2, 1).is_none());
        assert!(decode_gpu_identity(0xb, 0, 1).is_none());
        assert!(decode_gpu_identity(0x7, 4, 0).is_none());
        assert!(decode_gpu_identity(0x7, 4, 3).is_none());
    }
}

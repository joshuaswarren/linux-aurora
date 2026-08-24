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

        (0x7, 0) => (G15, P, 3, Legacy),
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

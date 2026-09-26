// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Allocation contract for independent G17P queues.
//! Every accepted DRM queue owns an execution context; hardware compute graphs
//! are allocated lazily when that logical queue first submits compute.
pub(crate) const FEATURE_INDEPENDENT_QUEUES: u64 = 1 << 2;
pub(crate) const PARAM_GROUP_QUEUE_LIMITS: u32 = 2;
pub(crate) const QUEUE_LIMITS_SIZE: usize = 16;
pub(crate) const TTBAT_CONTEXT_COUNT: u32 = 64;
pub(crate) const COMPUTE_CONTEXT_FIRST: u32 = 5;
/// The current exclusive renderer needs one duplicate app-context root.
pub(crate) const RENDER_APP_CONTEXT: u32 = TTBAT_CONTEXT_COUNT - 1;
/// Ordinary queues consume no second bind context. Context 1 permanently owns
/// the device-global render FList closure after bootstrap; contexts 5..62 are
/// dynamically allocated application-GART roots and context 63 remains the
/// driver-owned bootstrap render identity.
pub(crate) const BIND_CONTEXT_COUNT: u32 = 0;
pub(crate) const BIND_CONTEXT_FIRST: u32 = RENDER_APP_CONTEXT - BIND_CONTEXT_COUNT;
pub(crate) const BIND_CONTEXT_MASK: u64 =
    ((1u64 << RENDER_APP_CONTEXT) - 1) & !((1u64 << BIND_CONTEXT_FIRST) - 1);
pub(crate) const COMPUTE_CONTEXT_MASK: u64 =
    ((1u64 << BIND_CONTEXT_FIRST) - 1) & !((1u64 << COMPUTE_CONTEXT_FIRST) - 1);
pub(crate) const MAX_QUEUES: u32 = COMPUTE_CONTEXT_MASK.count_ones();
pub(crate) const MAX_IN_FLIGHT_PER_QUEUE: u32 = 1;
/// One GiB boundary inside the existing four GiB runtime reservation. The
/// canonical portion has room for all queues' 256-descriptor backing arrays
/// and auxiliary mappings; the remaining 3 GiB hold per-VM private aliases.
pub(crate) const CLIENT_LOW_VA_START: u64 = 0x70_4000_0000;

/// An idle ownerless graph may be rebound without reinstalling its QID.
///
/// QID routing names the session-owned queue/descriptor backing. The client
/// execution context and root live in the command descriptor and retained
/// binding, so exact completion followed by dropping that binding leaves the
/// installed graph reusable. This is the same retained-queue handoff the
/// synchronous path already uses; reinstalling the QID is explicitly wrong.
pub(crate) const fn reusable_idle_compute_graph(
    owner_free: bool,
    active_free: bool,
    binding_free: bool,
    context_free: bool,
    quarantined: bool,
) -> bool {
    owner_free && active_free && binding_free && context_free && !quarantined
}

/// An idle graph whose client has gone but whose binding is deliberately still
/// held, because the firmware has not been told to forget its QID.
///
/// The firmware keeps the installed slot pointer for a queue until something
/// clears it, and the only clearing transaction the driver has --
/// `unregister_compute_sksm_queue`'s disable pair -- is a suppressed no-op
/// whenever `g17p_sksm_mmio` is 0, which is its default because the SKSM port
/// raises an SError on this SoC. So a released queue's SKSM entry mapping and
/// context root must outlive the client and be dropped only where the
/// synchronous path already drops them: immediately before the next bind
/// reinstalls those roots, or after both firmware CPUs have stopped.
///
/// Such a graph is still reusable -- rebinding is exactly the boundary that
/// makes dropping the old binding safe -- so it must be found by the same
/// search, which is what this predicate is for.
pub(crate) const fn rebindable_released_compute_graph(
    owner_free: bool,
    active_free: bool,
    released: bool,
    quarantined: bool,
) -> bool {
    owner_free && active_free && released && !quarantined
}

pub(crate) fn encode_limits() -> [u8; QUEUE_LIMITS_SIZE] {
    let mut bytes = [0; QUEUE_LIMITS_SIZE];
    bytes[0..4].copy_from_slice(&MAX_QUEUES.to_ne_bytes());
    bytes[4..8].copy_from_slice(&MAX_IN_FLIGHT_PER_QUEUE.to_ne_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_graph_can_be_rebound_after_exact_owner_retirement() {
        assert!(reusable_idle_compute_graph(true, true, true, true, false));
    }

    #[test]
    fn active_owned_or_quarantined_graph_is_never_rebound() {
        assert!(!reusable_idle_compute_graph(false, true, true, true, false));
        assert!(!reusable_idle_compute_graph(true, false, true, true, false));
        assert!(!reusable_idle_compute_graph(true, true, false, true, false));
        assert!(!reusable_idle_compute_graph(true, true, true, false, false));
        assert!(!reusable_idle_compute_graph(true, true, true, true, true));
    }
}

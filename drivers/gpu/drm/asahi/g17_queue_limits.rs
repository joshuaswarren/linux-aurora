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
/// Hardware contexts reserved for user VM *bindings* above the opening pair.
///
/// A bound VM owns a hardware context outright: `Uat::bind` publishes its low
/// root there and the render path names that context as the fragment MCache
/// write-range selector, so two clients cannot share one. Context 1 is the
/// opening pair's and stays outside this band, which is why the first client
/// still lands on exactly the measured first-partial context. These IDs are
/// carved out of the compute execution band so the two allocators can never
/// hand out the same context.
pub(crate) const BIND_CONTEXT_COUNT: u32 = 3;
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

/// An ownerless graph may be rebound only while it is still host-prepared and
/// has never been installed in firmware. Once installed, its QID routing is
/// tied to the original execution context/root until a proven firmware
/// reconfigure transaction exists.
pub(crate) const fn reusable_uninstalled_compute_graph(
    owner_free: bool,
    active_free: bool,
    quarantined: bool,
    requires_first_install: bool,
) -> bool {
    owner_free && active_free && !quarantined && requires_first_install
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
    fn installed_graph_is_never_selected_for_cross_context_rebind() {
        assert!(!reusable_uninstalled_compute_graph(true, true, false, false));
    }

    #[test]
    fn unpublished_graph_can_be_rebound_after_idle_owner_close() {
        assert!(reusable_uninstalled_compute_graph(true, true, false, true));
    }

    #[test]
    fn active_owned_or_quarantined_graph_is_never_rebound() {
        assert!(!reusable_uninstalled_compute_graph(false, true, false, true));
        assert!(!reusable_uninstalled_compute_graph(true, false, false, true));
        assert!(!reusable_uninstalled_compute_graph(true, true, true, true));
    }
}

// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Pure policy for reversible T8140 dual-ASC lifecycle execution.
//!
//! The mailbox provider reports the grounded capability mask. This module
//! decides whether a live executor may cross the CPU-start boundary and fixes
//! the acquisition/cleanup order used by focused error-injection tests.

#![cfg_attr(not(test), allow(dead_code))]

/// A complete provider-owned stop/reset is not executable yet.
pub(crate) const TRANSACTIONAL_CPU_EXECUTOR_AVAILABLE: bool = false;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum LifecycleError {
    ProviderMissingStopReset,
    TransactionalExecutorUnavailable,
}

/// Admit CPU start only when both roles report no missing lifecycle pieces
/// and a real start+reverse-stop executor is present.
pub(crate) const fn admit_cpu_start(
    gfx_missing: u32,
    gfx1_missing: u32,
) -> Result<(), LifecycleError> {
    if gfx_missing != 0 || gfx1_missing != 0 {
        return Err(LifecycleError::ProviderMissingStopReset);
    }
    if !TRANSACTIONAL_CPU_EXECUTOR_AVAILABLE {
        return Err(LifecycleError::TransactionalExecutorUnavailable);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    enum Resource {
        GfxTransport,
        Gfx1Transport,
        GfxCpu,
        Gfx1Cpu,
    }

    const ACQUIRE_ORDER: [Resource; 4] = [
        Resource::GfxTransport,
        Resource::Gfx1Transport,
        Resource::GfxCpu,
        Resource::Gfx1Cpu,
    ];

    fn inject_failure_after(acquired: usize) -> (std::vec::Vec<Resource>, std::vec::Vec<Resource>) {
        let acquired = ACQUIRE_ORDER[..acquired].to_vec();
        let cleanup = acquired.iter().rev().copied().collect();
        (acquired, cleanup)
    }

    #[test]
    fn lifecycle_gate_rejects_each_role_and_the_absent_executor() {
        assert_eq!(
            admit_cpu_start(1, 0),
            Err(LifecycleError::ProviderMissingStopReset)
        );
        assert_eq!(
            admit_cpu_start(0, 1),
            Err(LifecycleError::ProviderMissingStopReset)
        );
        assert_eq!(
            admit_cpu_start(0, 0),
            Err(LifecycleError::TransactionalExecutorUnavailable)
        );
    }

    #[test]
    fn every_injected_partial_failure_cleans_up_in_reverse_order() {
        for acquired_count in 0..=ACQUIRE_ORDER.len() {
            let (acquired, cleanup) = inject_failure_after(acquired_count);
            assert_eq!(
                cleanup,
                acquired.iter().rev().copied().collect::<std::vec::Vec<_>>()
            );
        }

        let (_, full_cleanup) = inject_failure_after(ACQUIRE_ORDER.len());
        assert_eq!(
            full_cleanup,
            [
                Resource::Gfx1Cpu,
                Resource::GfxCpu,
                Resource::Gfx1Transport,
                Resource::GfxTransport,
            ]
        );
    }

    #[test]
    fn current_boundary_acquires_nothing() {
        let (acquired, cleanup) = inject_failure_after(0);
        assert!(acquired.is_empty());
        assert!(cleanup.is_empty());
        assert!(!TRANSACTIONAL_CPU_EXECUTOR_AVAILABLE);
    }
}

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

    /// Symbolic resources used only by the test executor below. There is no
    /// provider handle, FFI call, MMIO address, or production entry point in
    /// this model; it exists solely to pin transaction/unwind ordering.
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

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    enum Rollback {
        StopGfx1Cpu,
        StopGfxCpu,
        QuiesceGfx1Provider,
        QuiesceGfxProvider,
    }

    const FULL_ROLLBACK_ORDER: [Rollback; 4] = [
        Rollback::StopGfx1Cpu,
        Rollback::StopGfxCpu,
        Rollback::QuiesceGfx1Provider,
        Rollback::QuiesceGfxProvider,
    ];

    const fn rollback(resource: Resource) -> Rollback {
        match resource {
            Resource::GfxTransport => Rollback::QuiesceGfxProvider,
            Resource::Gfx1Transport => Rollback::QuiesceGfx1Provider,
            Resource::GfxCpu => Rollback::StopGfxCpu,
            Resource::Gfx1Cpu => Rollback::StopGfx1Cpu,
        }
    }

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    enum TestEvent {
        Acquire(Resource),
        InjectFailureAfter(usize),
        Rollback(Rollback),
    }

    struct TestOutcome {
        completed_body: bool,
        trace: std::vec::Vec<TestEvent>,
        live: std::vec::Vec<Resource>,
    }

    /// Test-only transactional boundary. `fail_after` injects an error after
    /// exactly that many symbolic acquisitions (including the body boundary
    /// after all four). Every exit consumes the live stack in reverse order.
    fn execute_test_only_transaction(fail_after: Option<usize>) -> TestOutcome {
        assert!(fail_after.is_none_or(|point| point <= ACQUIRE_ORDER.len()));

        let mut trace = std::vec::Vec::new();
        let mut live = std::vec::Vec::new();
        let mut completed_body = false;

        for resource in ACQUIRE_ORDER {
            if fail_after == Some(live.len()) {
                trace.push(TestEvent::InjectFailureAfter(live.len()));
                break;
            }
            trace.push(TestEvent::Acquire(resource));
            live.push(resource);
        }

        if live.len() == ACQUIRE_ORDER.len() {
            if fail_after == Some(live.len()) {
                trace.push(TestEvent::InjectFailureAfter(live.len()));
            } else {
                completed_body = true;
            }
        }

        while let Some(resource) = live.pop() {
            trace.push(TestEvent::Rollback(rollback(resource)));
        }

        TestOutcome {
            completed_body,
            trace,
            live,
        }
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
    fn test_only_executor_unwinds_every_injected_boundary() {
        for acquired_count in 0..=ACQUIRE_ORDER.len() {
            let outcome = execute_test_only_transaction(Some(acquired_count));
            assert!(!outcome.completed_body);
            assert!(outcome.live.is_empty());

            let rollback_trace = outcome
                .trace
                .iter()
                .filter_map(|event| match event {
                    TestEvent::Rollback(step) => Some(*step),
                    _ => None,
                })
                .collect::<std::vec::Vec<_>>();
            let expected = ACQUIRE_ORDER[..acquired_count]
                .iter()
                .rev()
                .copied()
                .map(rollback)
                .collect::<std::vec::Vec<_>>();
            assert_eq!(rollback_trace, expected);
        }
    }

    #[test]
    fn full_test_only_transaction_uses_proven_role_and_provider_order() {
        let outcome = execute_test_only_transaction(None);
        assert!(outcome.completed_body);
        assert!(outcome.live.is_empty());
        assert_eq!(
            outcome.trace,
            [
                TestEvent::Acquire(Resource::GfxTransport),
                TestEvent::Acquire(Resource::Gfx1Transport),
                TestEvent::Acquire(Resource::GfxCpu),
                TestEvent::Acquire(Resource::Gfx1Cpu),
                TestEvent::Rollback(Rollback::StopGfx1Cpu),
                TestEvent::Rollback(Rollback::StopGfxCpu),
                TestEvent::Rollback(Rollback::QuiesceGfx1Provider),
                TestEvent::Rollback(Rollback::QuiesceGfxProvider),
            ]
        );
        let rollback_trace = outcome.trace[4..]
            .iter()
            .map(|event| match event {
                TestEvent::Rollback(step) => *step,
                _ => panic!("non-rollback event after transaction body"),
            })
            .collect::<std::vec::Vec<_>>();
        assert_eq!(rollback_trace, FULL_ROLLBACK_ORDER);
    }

    #[test]
    fn current_boundary_acquires_nothing() {
        let outcome = execute_test_only_transaction(Some(0));
        assert_eq!(outcome.trace, [TestEvent::InjectFailureAfter(0)]);
        assert!(outcome.live.is_empty());
        assert!(!TRANSACTIONAL_CPU_EXECUTOR_AVAILABLE);
    }
}

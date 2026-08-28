// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Reversible T8140 dual-ASC CPU execution.

#![cfg_attr(not(test), allow(dead_code))]

/// Both provider roles now have matching start and stop operations.
pub(crate) const TRANSACTIONAL_CPU_EXECUTOR_AVAILABLE: bool = true;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum LifecycleError {
    ProviderLifecycleIncomplete,
}

/// Admit CPU start only when both providers report a complete lifecycle.
pub(crate) const fn admit_cpu_start(
    gfx_missing: u32,
    gfx1_missing: u32,
) -> Result<(), LifecycleError> {
    if gfx_missing != 0 || gfx1_missing != 0 {
        return Err(LifecycleError::ProviderLifecycleIncomplete);
    }

    Ok(())
}

/// One provider-owned ASC CPU lifecycle.
pub(crate) trait AscCpuLifecycle {
    type Error;

    fn start_cpu(&self) -> Result<(), Self::Error>;
    fn stop_cpu(&self) -> Result<(), Self::Error>;
}

/// Start GFX then GFX1, run `body`, and stop GFX1 then GFX.
///
/// Every path after GFX starts attempts its matching stop. A GFX1 start error
/// stops GFX before returning. Once both starts succeed, both stops run even
/// if the body or the first stop reports an error.
pub(crate) fn execute_cpu_pair<P, F>(gfx: &P, gfx1: &P, body: F) -> Result<(), P::Error>
where
    P: AscCpuLifecycle,
    F: FnOnce() -> Result<(), P::Error>,
{
    gfx.start_cpu()?;
    if let Err(err) = gfx1.start_cpu() {
        let _ = gfx.stop_cpu();
        return Err(err);
    }

    let body_result = body();
    let gfx1_stop = gfx1.stop_cpu();
    let gfx_stop = gfx.stop_cpu();

    match (body_result, gfx1_stop, gfx_stop) {
        (Err(err), _, _) | (Ok(()), Err(err), _) | (Ok(()), Ok(()), Err(err)) => Err(err),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    enum Role {
        Gfx,
        Gfx1,
    }

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    enum Event {
        Start(Role),
        Body,
        Stop(Role),
    }

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    enum Failure {
        StartGfx,
        StartGfx1,
        Body,
        StopGfx1,
        StopGfx,
    }

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    enum TestError {
        StartGfx,
        StartGfx1,
        Body,
        StopGfx1,
        StopGfx,
    }

    struct State {
        failure: Option<Failure>,
        trace: RefCell<std::vec::Vec<Event>>,
    }

    struct MockCpu<'a> {
        role: Role,
        state: &'a State,
    }

    impl AscCpuLifecycle for MockCpu<'_> {
        type Error = TestError;

        fn start_cpu(&self) -> Result<(), Self::Error> {
            self.state.trace.borrow_mut().push(Event::Start(self.role));
            match (self.role, self.state.failure) {
                (Role::Gfx, Some(Failure::StartGfx)) => Err(TestError::StartGfx),
                (Role::Gfx1, Some(Failure::StartGfx1)) => Err(TestError::StartGfx1),
                _ => Ok(()),
            }
        }

        fn stop_cpu(&self) -> Result<(), Self::Error> {
            self.state.trace.borrow_mut().push(Event::Stop(self.role));
            match (self.role, self.state.failure) {
                (Role::Gfx1, Some(Failure::StopGfx1)) => Err(TestError::StopGfx1),
                (Role::Gfx, Some(Failure::StopGfx)) => Err(TestError::StopGfx),
                _ => Ok(()),
            }
        }
    }

    fn run(failure: Option<Failure>) -> (Result<(), TestError>, std::vec::Vec<Event>) {
        let state = State {
            failure,
            trace: RefCell::new(std::vec::Vec::new()),
        };
        let gfx = MockCpu {
            role: Role::Gfx,
            state: &state,
        };
        let gfx1 = MockCpu {
            role: Role::Gfx1,
            state: &state,
        };

        let result = execute_cpu_pair(&gfx, &gfx1, || {
            state.trace.borrow_mut().push(Event::Body);
            if failure == Some(Failure::Body) {
                Err(TestError::Body)
            } else {
                Ok(())
            }
        });

        (result, state.trace.into_inner())
    }

    #[test]
    fn lifecycle_gate_requires_both_complete_providers() {
        assert_eq!(admit_cpu_start(0, 0), Ok(()));
        assert_eq!(
            admit_cpu_start(1, 0),
            Err(LifecycleError::ProviderLifecycleIncomplete)
        );
        assert_eq!(
            admit_cpu_start(0, 1),
            Err(LifecycleError::ProviderLifecycleIncomplete)
        );
        assert!(TRANSACTIONAL_CPU_EXECUTOR_AVAILABLE);
    }

    #[test]
    fn successful_pair_uses_gfx_then_gfx1_and_stops_in_reverse() {
        let (result, trace) = run(None);
        assert_eq!(result, Ok(()));
        assert_eq!(
            trace,
            [
                Event::Start(Role::Gfx),
                Event::Start(Role::Gfx1),
                Event::Body,
                Event::Stop(Role::Gfx1),
                Event::Stop(Role::Gfx),
            ]
        );
    }

    #[test]
    fn gfx_start_error_has_nothing_to_unwind() {
        let (result, trace) = run(Some(Failure::StartGfx));
        assert_eq!(result, Err(TestError::StartGfx));
        assert_eq!(trace, [Event::Start(Role::Gfx)]);
    }

    #[test]
    fn gfx1_start_error_stops_gfx() {
        let (result, trace) = run(Some(Failure::StartGfx1));
        assert_eq!(result, Err(TestError::StartGfx1));
        assert_eq!(
            trace,
            [
                Event::Start(Role::Gfx),
                Event::Start(Role::Gfx1),
                Event::Stop(Role::Gfx),
            ]
        );
    }

    #[test]
    fn body_error_stops_both_roles_in_reverse() {
        let (result, trace) = run(Some(Failure::Body));
        assert_eq!(result, Err(TestError::Body));
        assert_eq!(
            trace,
            [
                Event::Start(Role::Gfx),
                Event::Start(Role::Gfx1),
                Event::Body,
                Event::Stop(Role::Gfx1),
                Event::Stop(Role::Gfx),
            ]
        );
    }

    #[test]
    fn gfx1_stop_error_still_stops_gfx() {
        let (result, trace) = run(Some(Failure::StopGfx1));
        assert_eq!(result, Err(TestError::StopGfx1));
        assert_eq!(
            trace,
            [
                Event::Start(Role::Gfx),
                Event::Start(Role::Gfx1),
                Event::Body,
                Event::Stop(Role::Gfx1),
                Event::Stop(Role::Gfx),
            ]
        );
    }

    #[test]
    fn gfx_stop_error_is_reported_after_both_stops() {
        let (result, trace) = run(Some(Failure::StopGfx));
        assert_eq!(result, Err(TestError::StopGfx));
        assert_eq!(
            trace,
            [
                Event::Start(Role::Gfx),
                Event::Start(Role::Gfx1),
                Event::Body,
                Event::Stop(Role::Gfx1),
                Event::Stop(Role::Gfx),
            ]
        );
    }
}

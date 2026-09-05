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

/// Owned pair of provider handles whose CPUs are running.
///
/// Construction starts GFX before GFX1. Dropping the owner always attempts to
/// stop GFX1 before GFX, so a platform driver can retain the pair for its
/// entire bound lifetime without duplicating partial-start rollback logic.
pub(crate) struct StartedCpuPair<P: AscCpuLifecycle> {
    gfx: P,
    gfx1: P,
    gfx_running: bool,
    gfx1_running: bool,
}

impl<P: AscCpuLifecycle> StartedCpuPair<P> {
    /// Start both provider-owned CPUs and return their retained owner.
    pub(crate) fn start(gfx: P, gfx1: P) -> Result<Self, P::Error> {
        Self::start_staged(gfx, gfx1, || Ok(()))
    }

    /// Start GFX and complete its RTKit-ready callback before starting GFX1.
    ///
    /// This is the qualified Linux policy for its synchronous RTKit boot
    /// callback. Native `AGXArmFirmware::bootFirmware` requests role 0 before
    /// role 1, but its observed RUN writes vary: valid 25G83 capture 092506
    /// starts GFX first, while 194053 starts GFX1 first. Those native requests
    /// therefore do not establish a universal primary-handshake-first rule.
    /// Retain the Linux order tested through 1,000 renders in one cold session, with
    /// rollback stopping only the processors whose starts have succeeded.
    pub(crate) fn start_staged<F>(gfx: P, gfx1: P, gfx_ready: F) -> Result<Self, P::Error>
    where
        F: FnOnce() -> Result<(), P::Error>,
    {
        gfx.start_cpu()?;
        if let Err(err) = gfx_ready() {
            let _ = gfx.stop_cpu();
            return Err(err);
        }
        if let Err(err) = gfx1.start_cpu() {
            let _ = gfx.stop_cpu();
            return Err(err);
        }

        Ok(Self {
            gfx,
            gfx1,
            gfx_running: true,
            gfx1_running: true,
        })
    }

    /// Stop both processors and retain their provider handles.
    ///
    /// A successful return is the boundary after which firmware can no longer
    /// access queue, descriptor, timestamp, or translation objects. Both stop
    /// calls run even if the first reports an error.
    pub(crate) fn stop(&mut self) -> Result<(), P::Error> {
        if !self.gfx_running && !self.gfx1_running {
            return Ok(());
        }

        let mut first_error = None;
        if self.gfx1_running {
            match self.gfx1.stop_cpu() {
                Ok(()) => self.gfx1_running = false,
                Err(err) => first_error = Some(err),
            }
        }
        if self.gfx_running {
            match self.gfx.stop_cpu() {
                Ok(()) => self.gfx_running = false,
                Err(err) if first_error.is_none() => first_error = Some(err),
                Err(_) => {}
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Borrow the retained primary provider while both lifecycle owners stay
    /// inside this pair.
    pub(crate) fn gfx(&self) -> &P {
        &self.gfx
    }
}

impl<P: AscCpuLifecycle> Drop for StartedCpuPair<P> {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Operations for replacing a timed-out persistent firmware session.
///
/// The old graph stays alive until both processors stop. A failed rebuild
/// leaves submissions paused so the caller can retry the complete rebuild.
pub(crate) trait PersistentRuntimeRecovery {
    type Error;

    fn pause_submissions(&mut self);
    fn stop_cpu_pair(&mut self) -> Result<(), Self::Error>;
    fn drop_gfx1_rtkit(&mut self);
    fn drop_gfx_rtkit(&mut self);
    /// Disable the hardware-visible SKSM queue while retaining its storage.
    fn disable_sksm_queue(&mut self) -> Result<(), Self::Error>;
    /// Drop per-session queue and render state while retaining manager/UAT.
    fn release_runtime_objects(&mut self);
    /// Restore initdata and transports on the retained manager/UAT.
    fn rebuild_session_in_place(&mut self) -> Result<(), Self::Error>;
    fn resume_submissions(&mut self);
}

/// Stop both processors, release the old graph, and build fresh initdata.
pub(crate) fn recover_persistent_runtime<T: PersistentRuntimeRecovery>(
    owner: &mut T,
) -> Result<(), T::Error> {
    owner.pause_submissions();
    owner.stop_cpu_pair()?;
    owner.drop_gfx1_rtkit();
    owner.drop_gfx_rtkit();
    owner.disable_sksm_queue()?;
    owner.release_runtime_objects();
    owner.rebuild_session_in_place()?;
    owner.resume_submissions();
    Ok(())
}

/// Decide whether the next render must start a fresh firmware session.
/// Physical 256-slot SKSM/outer rings wrap without a session restart. The
/// current event writer admits 24-bit stamps; retain that real width bound.
/// A VM change still requires a cold session because the graph has an owner.
pub(crate) const fn retained_render_requires_recycle(
    retained_graph: bool,
    same_vm: bool,
    next_ordinal: u32,
) -> bool {
    retained_graph && (!same_vm || next_ordinal >= 0x00ff_ffff)
}

/// Operations needed to dismantle a retained G17P runtime.
///
/// Keeping the ordering in this testable helper makes the platform owner's
/// `Drop` path explicit: both CPUs stop before any transport, queue, binding,
/// or mapping can disappear. Manager/UAT resources are released last.
pub(crate) trait PersistentRuntimeTeardown {
    /// Stop both firmware processors. No firmware-visible owner may drop
    /// unless this succeeds.
    fn stop_cpu_pair(&mut self) -> bool;
    /// Preserve every firmware-visible owner when CPU stop cannot be proven.
    fn retain_after_stop_failure(&mut self);
    fn unregister_queue(&mut self);
    fn drop_queue(&mut self);
    fn drop_gfx1_rtkit(&mut self);
    fn drop_gfx_rtkit(&mut self);
    fn drop_cpu_pair(&mut self);
    fn drop_manager(&mut self);
}

/// Tear down retained runtime state after proving firmware cannot dereference it.
pub(crate) fn teardown_persistent_runtime<T: PersistentRuntimeTeardown>(owner: &mut T) {
    if !owner.stop_cpu_pair() {
        owner.retain_after_stop_failure();
        return;
    }
    owner.unregister_queue();
    owner.drop_gfx1_rtkit();
    owner.drop_gfx_rtkit();
    owner.drop_queue();
    owner.drop_cpu_pair();
    owner.drop_manager();
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
        UnregisterQueue,
        DropQueue,
        DropGfx1Rtkit,
        DropGfxRtkit,
        DropCpuPair,
        DropManager,
        RetainRuntime,
        PauseSubmissions,
        StopCpuPair,
        DisableSksmQueue,
        ReleaseRuntimeObjects,
        RebuildSessionInPlace,
        ResumeSubmissions,
    }

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    enum Failure {
        StartGfx,
        StartGfx1,
        Body,
        StopGfx1,
        StopGfx,
        StopPair,
        RebuildSession,
        DisableSksm,
    }

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    enum TestError {
        StartGfx,
        StartGfx1,
        Body,
        StopGfx1,
        StopGfx,
        StopPair,
        RebuildSession,
        DisableSksm,
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

    #[test]
    fn retained_pair_stays_started_until_owner_drop() {
        let state = State {
            failure: None,
            trace: RefCell::new(std::vec::Vec::new()),
        };
        let pair = StartedCpuPair::start(
            MockCpu {
                role: Role::Gfx,
                state: &state,
            },
            MockCpu {
                role: Role::Gfx1,
                state: &state,
            },
        )
        .unwrap();

        assert_eq!(
            *state.trace.borrow(),
            [Event::Start(Role::Gfx), Event::Start(Role::Gfx1)]
        );
        drop(pair);
        assert_eq!(
            state.trace.into_inner(),
            [
                Event::Start(Role::Gfx),
                Event::Start(Role::Gfx1),
                Event::Stop(Role::Gfx1),
                Event::Stop(Role::Gfx),
            ]
        );
    }

    #[test]
    fn staged_pair_finishes_primary_handshake_before_secondary_start() {
        let state = State {
            failure: None,
            trace: RefCell::new(std::vec::Vec::new()),
        };
        let pair = StartedCpuPair::start_staged(
            MockCpu {
                role: Role::Gfx,
                state: &state,
            },
            MockCpu {
                role: Role::Gfx1,
                state: &state,
            },
            || {
                state.trace.borrow_mut().push(Event::Body);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            *state.trace.borrow(),
            [
                Event::Start(Role::Gfx),
                Event::Body,
                Event::Start(Role::Gfx1),
            ]
        );
        drop(pair);
    }

    #[test]
    fn staged_pair_stops_primary_when_its_handshake_fails() {
        let state = State {
            failure: None,
            trace: RefCell::new(std::vec::Vec::new()),
        };
        let result = StartedCpuPair::start_staged(
            MockCpu {
                role: Role::Gfx,
                state: &state,
            },
            MockCpu {
                role: Role::Gfx1,
                state: &state,
            },
            || {
                state.trace.borrow_mut().push(Event::Body);
                Err(TestError::Body)
            },
        );

        assert!(matches!(result, Err(TestError::Body)));
        assert_eq!(
            *state.trace.borrow(),
            [
                Event::Start(Role::Gfx),
                Event::Body,
                Event::Stop(Role::Gfx),
            ]
        );
    }

    #[test]
    fn explicitly_stopped_pair_does_not_stop_twice_on_drop() {
        let state = State {
            failure: None,
            trace: RefCell::new(std::vec::Vec::new()),
        };
        let mut pair = StartedCpuPair::start(
            MockCpu {
                role: Role::Gfx,
                state: &state,
            },
            MockCpu {
                role: Role::Gfx1,
                state: &state,
            },
        )
        .unwrap();

        assert_eq!(pair.stop(), Ok(()));
        assert_eq!(pair.stop(), Ok(()));
        drop(pair);
        assert_eq!(
            state.trace.into_inner(),
            [
                Event::Start(Role::Gfx),
                Event::Start(Role::Gfx1),
                Event::Stop(Role::Gfx1),
                Event::Stop(Role::Gfx),
            ]
        );
    }

    #[test]
    fn partial_stop_retry_touches_only_the_role_that_did_not_stop() {
        let state = State {
            failure: Some(Failure::StopGfx1),
            trace: RefCell::new(std::vec::Vec::new()),
        };
        let mut pair = StartedCpuPair::start(
            MockCpu {
                role: Role::Gfx,
                state: &state,
            },
            MockCpu {
                role: Role::Gfx1,
                state: &state,
            },
        )
        .unwrap();

        assert_eq!(pair.stop(), Err(TestError::StopGfx1));
        drop(pair);
        assert_eq!(
            state.trace.into_inner(),
            [
                Event::Start(Role::Gfx),
                Event::Start(Role::Gfx1),
                Event::Stop(Role::Gfx1),
                Event::Stop(Role::Gfx),
                Event::Stop(Role::Gfx1),
            ]
        );
    }

    #[test]
    fn retained_pair_rolls_back_primary_when_secondary_start_fails() {
        let state = State {
            failure: Some(Failure::StartGfx1),
            trace: RefCell::new(std::vec::Vec::new()),
        };
        let result = StartedCpuPair::start(
            MockCpu {
                role: Role::Gfx,
                state: &state,
            },
            MockCpu {
                role: Role::Gfx1,
                state: &state,
            },
        );

        match result {
            Err(TestError::StartGfx1) => {}
            Err(other) => panic!("unexpected start error: {other:?}"),
            Ok(pair) => {
                drop(pair);
                panic!("secondary start unexpectedly succeeded");
            }
        }
        assert_eq!(
            *state.trace.borrow(),
            [
                Event::Start(Role::Gfx),
                Event::Start(Role::Gfx1),
                Event::Stop(Role::Gfx),
            ]
        );
    }

    struct MockRuntimeTeardown {
        trace: std::vec::Vec<Event>,
        stop_ok: bool,
    }

    impl PersistentRuntimeTeardown for MockRuntimeTeardown {
        fn stop_cpu_pair(&mut self) -> bool {
            self.trace.push(Event::StopCpuPair);
            self.stop_ok
        }

        fn retain_after_stop_failure(&mut self) {
            self.trace.push(Event::RetainRuntime);
        }

        fn unregister_queue(&mut self) {
            self.trace.push(Event::UnregisterQueue);
        }

        fn drop_queue(&mut self) {
            self.trace.push(Event::DropQueue);
        }

        fn drop_gfx1_rtkit(&mut self) {
            self.trace.push(Event::DropGfx1Rtkit);
        }

        fn drop_gfx_rtkit(&mut self) {
            self.trace.push(Event::DropGfxRtkit);
        }

        fn drop_cpu_pair(&mut self) {
            self.trace.push(Event::DropCpuPair);
        }

        fn drop_manager(&mut self) {
            self.trace.push(Event::DropManager);
        }
    }

    #[test]
    fn persistent_runtime_teardown_is_strictly_reverse_ordered() {
        let mut owner = MockRuntimeTeardown {
            trace: std::vec::Vec::new(),
            stop_ok: true,
        };
        teardown_persistent_runtime(&mut owner);
        assert_eq!(
            owner.trace,
            [
                Event::StopCpuPair,
                Event::UnregisterQueue,
                Event::DropGfx1Rtkit,
                Event::DropGfxRtkit,
                Event::DropQueue,
                Event::DropCpuPair,
                Event::DropManager,
            ]
        );
    }

    #[test]
    fn persistent_runtime_teardown_retains_every_owner_when_stop_fails() {
        let mut owner = MockRuntimeTeardown {
            trace: std::vec::Vec::new(),
            stop_ok: false,
        };
        teardown_persistent_runtime(&mut owner);
        assert_eq!(owner.trace, [Event::StopCpuPair, Event::RetainRuntime]);
    }

    struct MockRuntimeRecovery {
        failure: Option<Failure>,
        trace: std::vec::Vec<Event>,
        manager_uat_id: u64,
    }

    impl PersistentRuntimeRecovery for MockRuntimeRecovery {
        type Error = TestError;

        fn pause_submissions(&mut self) {
            self.trace.push(Event::PauseSubmissions);
        }

        fn stop_cpu_pair(&mut self) -> Result<(), Self::Error> {
            self.trace.push(Event::StopCpuPair);
            if self.failure == Some(Failure::StopPair) {
                Err(TestError::StopPair)
            } else {
                Ok(())
            }
        }

        fn drop_gfx1_rtkit(&mut self) {
            self.trace.push(Event::DropGfx1Rtkit);
        }

        fn drop_gfx_rtkit(&mut self) {
            self.trace.push(Event::DropGfxRtkit);
        }

        fn disable_sksm_queue(&mut self) -> Result<(), Self::Error> {
            self.trace.push(Event::DisableSksmQueue);
            if self.failure == Some(Failure::DisableSksm) {
                Err(TestError::DisableSksm)
            } else {
                Ok(())
            }
        }

        fn release_runtime_objects(&mut self) {
            self.trace.push(Event::ReleaseRuntimeObjects);
        }

        fn rebuild_session_in_place(&mut self) -> Result<(), Self::Error> {
            self.trace.push(Event::RebuildSessionInPlace);
            if self.failure == Some(Failure::RebuildSession) {
                Err(TestError::RebuildSession)
            } else {
                Ok(())
            }
        }

        fn resume_submissions(&mut self) {
            self.trace.push(Event::ResumeSubmissions);
        }
    }

    #[test]
    fn timeout_recovery_stops_both_processors_before_releasing_the_graph() {
        let mut owner = MockRuntimeRecovery {
            failure: None,
            trace: std::vec::Vec::new(),
            manager_uat_id: 0x9000_0000,
        };
        let manager_uat_id = owner.manager_uat_id;
        assert_eq!(recover_persistent_runtime(&mut owner), Ok(()));
        assert_eq!(owner.manager_uat_id, manager_uat_id);
        assert_eq!(
            owner.trace,
            [
                Event::PauseSubmissions,
                Event::StopCpuPair,
                Event::DropGfx1Rtkit,
                Event::DropGfxRtkit,
                Event::DisableSksmQueue,
                Event::ReleaseRuntimeObjects,
                Event::RebuildSessionInPlace,
                Event::ResumeSubmissions,
            ]
        );
    }

    #[test]
    fn processor_stop_error_keeps_every_firmware_owned_object_alive() {
        let mut owner = MockRuntimeRecovery {
            failure: Some(Failure::StopPair),
            trace: std::vec::Vec::new(),
            manager_uat_id: 0x9000_0000,
        };
        assert_eq!(
            recover_persistent_runtime(&mut owner),
            Err(TestError::StopPair)
        );
        assert_eq!(owner.trace, [Event::PauseSubmissions, Event::StopCpuPair]);
    }

    #[test]
    fn rebuild_error_leaves_submissions_paused_after_safe_release() {
        let mut owner = MockRuntimeRecovery {
            failure: Some(Failure::RebuildSession),
            trace: std::vec::Vec::new(),
            manager_uat_id: 0x9000_0000,
        };
        assert_eq!(
            recover_persistent_runtime(&mut owner),
            Err(TestError::RebuildSession)
        );
        assert_eq!(
            owner.trace,
            [
                Event::PauseSubmissions,
                Event::StopCpuPair,
                Event::DropGfx1Rtkit,
                Event::DropGfxRtkit,
                Event::DisableSksmQueue,
                Event::ReleaseRuntimeObjects,
                Event::RebuildSessionInPlace,
            ]
        );
    }

    #[test]
    fn sksm_disable_error_keeps_queue_and_graph_storage_alive() {
        let mut owner = MockRuntimeRecovery {
            failure: Some(Failure::DisableSksm),
            trace: std::vec::Vec::new(),
            manager_uat_id: 0x9000_0000,
        };
        assert_eq!(
            recover_persistent_runtime(&mut owner),
            Err(TestError::DisableSksm)
        );
        assert_eq!(
            owner.trace,
            [
                Event::PauseSubmissions,
                Event::StopCpuPair,
                Event::DropGfx1Rtkit,
                Event::DropGfxRtkit,
                Event::DisableSksmQueue,
            ]
        );
    }

    #[test]
    fn physical_ring_wraps_do_not_rebuild_the_retained_session() {
        for ordinal in [255, 256, 257, 426, 427, 999, 1000, 0x00ff_fffe] {
            assert!(!retained_render_requires_recycle(true, true, ordinal));
            assert!(retained_render_requires_recycle(true, false, ordinal));
        }
        let mut next_ordinal = 0x00ff_fffe;
        assert!(!retained_render_requires_recycle(true, true, next_ordinal));
        next_ordinal += 1;
        assert!(retained_render_requires_recycle(true, true, next_ordinal));

        let mut owner = MockRuntimeRecovery {
            failure: None,
            trace: std::vec::Vec::new(),
            manager_uat_id: 0x9000_0000,
        };
        let manager_uat_id = owner.manager_uat_id;
        assert_eq!(recover_persistent_runtime(&mut owner), Ok(()));
        next_ordinal = 0;
        assert_eq!(owner.manager_uat_id, manager_uat_id);
        assert!(!retained_render_requires_recycle(false, true, next_ordinal));
        assert_eq!(owner.trace.last(), Some(&Event::ResumeSubmissions));
    }
}

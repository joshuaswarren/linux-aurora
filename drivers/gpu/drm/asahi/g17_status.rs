// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! CPU-owned sticky VM failure reporting. No GPU mapping or firmware access.
use core::sync::atomic::{AtomicI32, Ordering};
use kernel::prelude::*;

pub(crate) const PARAM_GROUP_VM_STATUS: u32 = 1;
pub(crate) const FEATURE_VM_STATUS: u64 = 1 << 1;
pub(crate) const VM_STATUS_SIZE: usize = 16;

/// Shared by the VM and accepted jobs; never cleared by queue destruction or
/// successful work. A newly created VM receives a new status object.
pub(crate) struct VmStatus { error: AtomicI32 }
impl VmStatus {
    pub(crate) fn new() -> Self { Self { error: AtomicI32::new(0) } }
    /// Poisoning a VM status is terminal for that client: every later
    /// submission refuses at `stage=vm-status`.  Until now nothing said WHO
    /// poisoned it, so a wedged machine showed 114 `stage=vm-status` failures
    /// across 135 client files with no way to attribute a single one.  Name the
    /// caller on the 0 -> non-zero transition only, so this is one line per VM
    /// that actually dies rather than per submission.
    /// Does this error mean "this work will never run on this device"?
    ///
    /// Only those may be recorded on a VM's sticky status, because that status
    /// permanently retires the VM: every later submission refuses at
    /// `stage=vm-status`. `G17PQueue::note_submission_failure` already states
    /// the policy -- "Contention and rejection do not: an EBUSY from the render
    /// lease, an interrupted wait, or a malformed command buffer all leave a
    /// perfectly healthy VM, and recording a sticky error for one of those
    /// would retire a client that merely lost a frame" -- but two paths
    /// recorded unconditionally and so contradicted it.
    ///
    /// Measured on G17P 2026-09-18: a compute submission arriving while a
    /// firmware recovery was still open logged "firmware recovery still open
    /// after 200 polls; refusing to publish" and then poisoned its VM with
    /// EBUSY. Recovery is transient by definition. 52 further VMs were poisoned
    /// in the cascade and the GPU was dead until reboot.
    pub(crate) fn is_device_loss(error: i32) -> bool {
        error == -(kernel::error::code::EIO.to_errno().unsigned_abs() as i32)
            || error == -(kernel::error::code::ETIMEDOUT.to_errno().unsigned_abs() as i32)
            || error == -(kernel::error::code::ENODEV.to_errno().unsigned_abs() as i32)
    }

    /// Record only if the error actually means the device is gone for this VM.
    /// The caller still reports the error to its own client through the fence.
    #[track_caller]
    pub(crate) fn record_if_device_loss(&self, error: i32) {
        if Self::is_device_loss(error) {
            self.record(error);
        }
    }

    #[track_caller]
    pub(crate) fn record(&self, error: i32) {
        if error < 0 {
            if self
                .error
                .compare_exchange(0, error, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let loc = core::panic::Location::caller();
                pr_err!(
                    "G17PMARK vm-status POISONED err={} by {}:{}\n",
                    error,
                    loc.file(),
                    loc.line(),
                );
            }
        }
    }
    pub(crate) fn get(&self) -> i32 { self.error.load(Ordering::Acquire) }
}

/// GET_PARAMS group 1 is an in/out request: vm_id, flags=0, error(out), pad=0.
/// Decode once, before lookup, so a racing userspace edit cannot change owner.
pub(crate) fn decode_request(bytes: &[u8; VM_STATUS_SIZE]) -> Option<u32> {
    let word = |i| u32::from_ne_bytes(bytes[i..i+4].try_into().unwrap());
    if word(4) != 0 || word(12) != 0 { return None; }
    Some(word(0))
}
pub(crate) fn encode_response(vm_id: u32, error: i32) -> [u8; VM_STATUS_SIZE] {
    let mut bytes = [0; VM_STATUS_SIZE];
    bytes[0..4].copy_from_slice(&vm_id.to_ne_bytes());
    bytes[8..12].copy_from_slice(&error.to_ne_bytes());
    bytes
}

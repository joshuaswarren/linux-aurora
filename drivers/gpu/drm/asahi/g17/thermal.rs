// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! GPU power budget governor for M5 (J714).
//!
//! The firmware limits the GPU to the host power budget in Region C 0x88 within about a second
//! (the calibrated maximum is written there at initialization). Left at that maximum under
//! sustained load, the SoC hotspot temperature computed by the SMC (about 6 C above the hottest
//! GPU die sensor while the GPU dominates) spends time above roughly 97 C, and the firmware's
//! emergency path then pins the GPU to its base performance state for 20-40 s, repeatedly. This
//! governor reads the hotspot temperature through the thermal framework once per second while the
//! GPU is busy and, once it reaches the engage temperature, lowers the budget with a PI controller
//! (plus a feed-forward on the temperature's rise) so it settles at the target, below that region.
//!
//! The controller works below the GPU's actual draw (the calibrated maximum is far above it). It
//! keeps the integral it converged to across releases, so after a release or a firmware clamp it
//! resumes from that budget; after a long period without control the memory decays to the
//! cold-start budget, never to the ceiling. It holds while the firmware's emergency clamp is
//! active and never releases then. The default target keeps the hotspot below the level at which
//! the firmware clamps even with a heat-soaked chassis. As a safety net, each emergency clamp the
//! governor sees lowers its effective target by 1 C (at most 6 C, not below 84 C), and every 10
//! minutes without one raises it again by 1 C. It publishes the budget like the effort input, only when it changes, and
//! restores the calibrated maximum on release, when the GPU goes idle, when no sensor can be read
//! and at shutdown. Below the engage temperature nothing is written.

use core::ffi::{c_char, c_int, c_void, CStr};
use kernel::bindings;

extern "C" {
    fn thermal_zone_get_zone_by_name(name: *const c_char) -> *mut c_void;
    fn thermal_zone_get_temp(zone: *mut c_void, temp: *mut c_int) -> c_int;
}

/// SoC hotspot temperature computed by the SMC (registered as a thermal zone by the SMC driver):
/// the input of the firmware's emergency clamp.
const J714_HOTSPOT_ZONE: &CStr = c"smc-TCMb";
/// The hottest of the 42 SMC GPU die sensors of J714 near the limit, and their offset to the
/// hotspot while the GPU dominates: the fallback input when the hotspot cannot be read.
const J714_GPU_DIE_ZONES: [&CStr; 8] = [
    c"smc-Tg08", c"smc-Tg12", c"smc-Tg0O", c"smc-Tg0X", c"smc-Tg29", c"smc-Tg2P", c"smc-Tg1U",
    c"smc-Tg43",
];
const GPU_DIE_TO_HOTSPOT_MC: i32 = 6000;

/// Region C word holding the host GPU power budget (mW).
pub(crate) const REGION_C_POWER_BUDGET: usize = 0x88;
/// Shared power-policy word holding the GPU's current power draw (mW), written by the firmware.
pub(crate) const POLICY_GPU_POWER: usize = 0xad4;
/// Shared power-policy words of the firmware's primary performance limiter: the mask of active
/// limit reasons and the limit. The emergency reason with the limit at its floor is the clamp.
pub(crate) const POLICY_LIMITER_MASK: usize = 0x44;
pub(crate) const POLICY_LIMITER_LIMIT: usize = 0x48;
const LIMITER_EMERGENCY: u32 = 0x4;
const LIMITER_FLOOR: u32 = 100;
/// Control period while the GPU is busy.
pub(crate) const PERIOD_MS: u32 = 1000;
/// Restore the maximum after this long without a submission.
const IDLE_RESTORE_NS: u64 = 2_000_000_000;
/// Engage when the hotspot reaches this; release below it minus the hysteresis once the
/// controller has returned to its ceiling.
const ENGAGE_BELOW_TARGET_MC: i32 = 4000;
const RELEASE_HYSTERESIS_MC: i32 = 3000;
/// Lowest budget the governor sets (mW).
const FLOOR_MW: i64 = 10_000;
/// Lowest ceiling of the controller (mW): the GPU's draw at engage, but at least this.
const CEILING_MIN_MW: i64 = 40_000;
/// Proportional gain (mW per C) and integral gain (mW per C per second). Power follows the
/// budget within a second; the die settles over 30-60 s at ~1.2-1.5 C per W.
const KP_MW_PER_C: i64 = 1500;
const KI_MW_PER_C_S: i64 = 50;
/// Feed-forward on the hotspot's rise (mW per C/s), applied only while it rises, to curb the
/// overshoot when the GPU restarts from cool.
const KF_MW_PER_C_PER_S: i64 = 2000;
/// Without a converged integral, start at this fraction of the ceiling: the budget that holds
/// the target is about half the GPU's unconstrained draw.
const INITIAL_NUM: i64 = 3;
const INITIAL_DEN: i64 = 5;
/// The converged integral is kept for this long after the controller was last active, then
/// decays linearly to the cold-start integral over the decay time.
const MEMORY_HOLD_NS: u64 = 60_000_000_000;
const MEMORY_DECAY_NS: u64 = 300_000_000_000;
/// Effective target reduction per emergency clamp seen while governing, its limit, the lowest
/// effective target it leads to, and the time without a clamp after which it is reduced by one
/// step again.
const CLAMP_TARGET_STEP_MC: i32 = 1000;
const CLAMP_TARGET_MAX_MC: i32 = 6000;
const CLAMP_TARGET_FLOOR_MC: i32 = 84_000;
const CLAMP_TARGET_RECOVER_NS: u64 = 600_000_000_000;
/// Budget changes smaller than this are not published.
const STEP_MW: i64 = 250;

fn enabled() -> bool {
    *crate::module_parameters::m5_thermal_governor.value() & 1 != 0
}
pub(super) fn tracing() -> bool {
    *crate::module_parameters::m5_thermal_governor.value() & 2 != 0
}
pub(super) fn requested() -> bool {
    enabled() || tracing()
}
fn target_mc() -> i32 {
    let parameter = *crate::module_parameters::m5_thermal_target_mc.value();
    i32::try_from(parameter).unwrap_or(90_000).clamp(70_000, 104_000)
}

/// Whether the firmware's primary limiter words describe the emergency clamp.
pub(crate) fn is_emergency_clamp(mask: u32, limit: u32) -> bool {
    mask & LIMITER_EMERGENCY != 0 && limit <= LIMITER_FLOOR
}

/// Pins the sensor provider and serializes reads with its probe and removal.
/// Its thermal zones are managed by the hwmon device registered during probe;
/// removal releases that device's resources while holding the provider lock.
struct SensorProvider(*mut bindings::device);

impl SensorProvider {
    fn acquire() -> Option<Self> {
        // SAFETY: the bus and name have static lifetimes. The bus iterator
        // returns an owned device reference on success.
        let device = unsafe {
            bindings::bus_find_device(
                &bindings::platform_bus_type,
                core::ptr::null_mut(),
                c"macsmc-hwmon".as_ptr().cast(),
                Some(bindings::device_match_name),
            )
        };
        if device.is_null() {
            return None;
        }
        // SAFETY: the owned reference keeps the device and mutex alive. No
        // GPU driver lock is held during acquisition or sensor reads.
        unsafe { bindings::mutex_lock(&raw mut (*device).mutex) };
        let provider = Self(device);
        // SAFETY: the provider's lock protects binding and unbinding.
        if unsafe { (*device).driver.is_null() } {
            return None;
        }
        Some(provider)
    }
}

impl Drop for SensorProvider {
    fn drop(&mut self) {
        // SAFETY: acquire obtained this reference and locked this mutex on
        // the current thread; the guard never leaves the sensor-read call.
        unsafe {
            bindings::mutex_unlock(&raw mut (*self.0).mutex);
            bindings::put_device(self.0);
        }
    }
}

/// The provider guard prevents its managed zones from disappearing during
/// lookup and read. Do not retain a zone pointer after dropping the guard.
fn zone_temperature_mc(_provider: &SensorProvider, name: &CStr) -> Option<i32> {
    // SAFETY: name is a NUL-terminated string that outlives the call.
    let zone = unsafe { thermal_zone_get_zone_by_name(name.as_ptr()) };
    if zone.is_null() || (zone as usize) >= usize::MAX - 4095 {
        return None;
    }
    let mut temp: c_int = 0;
    // SAFETY: the provider guard protects the zone's lifetime, and temp is a
    // valid destination for this call.
    (unsafe { thermal_zone_get_temp(zone, &mut temp) } == 0)
        .then_some(temp)
        .filter(|temp| (0..=125_000).contains(temp))
}

/// SoC hotspot temperature in milli-C (the hottest GPU die sensor plus its offset when the hotspot
/// cannot be read), or None when no sensor can be read. Called without driver locks held: zone
/// lookups and sensor reads may sleep.
pub(crate) fn hotspot_temperature_mc() -> Option<i32> {
    let provider = SensorProvider::acquire()?;
    zone_temperature_mc(&provider, J714_HOTSPOT_ZONE).or_else(|| {
        J714_GPU_DIE_ZONES.iter().filter_map(|name| zone_temperature_mc(&provider, name)).max()
            .map(|temp| temp + GPU_DIE_TO_HOTSPOT_MC)
    })
}

pub(crate) struct Governor {
    maximum_mw: u32,
    budget_mw: u32,
    integral_mw: i64,
    /// Upper limit of the controller while active: the GPU's draw at engage.
    ceiling_mw: i64,
    /// Integral the controller last converged to (output inside its range), kept across releases.
    converged_mw: i64,
    active: bool,
    last_tick_ns: u64,
    last_activity_ns: u64,
    /// Last tick at which the controller was active, for the memory decay.
    last_active_ns: u64,
    last_temp_mc: Option<i32>,
    /// Effective target reduction learnt from emergency clamps, and when it last changed.
    target_offset_mc: i32,
    target_offset_ns: u64,
    was_clamped: bool,
}

impl Governor {
    pub(crate) const fn new() -> Self {
        Self { maximum_mw: 0, budget_mw: 0, integral_mw: 0, ceiling_mw: 0, converged_mw: 0, active: false,
            last_tick_ns: 0, last_activity_ns: 0, last_active_ns: 0, last_temp_mc: None, target_offset_mc: 0,
            target_offset_ns: 0, was_clamped: false }
    }

    /// The firmware accepted a submission; returns whether the control loop should run.
    pub(crate) fn note_activity(&mut self, now_ns: u64) -> bool {
        self.last_activity_ns = now_ns;
        requested() || self.budget_mw != self.maximum_mw
    }

    /// Learn the calibrated maximum from the budget word before the first change.
    pub(crate) fn set_maximum(&mut self, current_mw: u32) {
        if self.maximum_mw == 0 && i64::from(current_mw) >= FLOOR_MW {
            self.maximum_mw = current_mw;
            self.budget_mw = current_mw;
        }
    }

    /// The budget to publish while stopping, if it differs from the maximum.
    pub(crate) fn restore(&mut self) -> Option<u32> {
        self.active = false;
        self.integral_mw = 0;
        self.converged_mw = 0;
        self.target_offset_mc = 0;
        if self.maximum_mw == 0 || self.budget_mw == self.maximum_mw {
            return None;
        }
        self.budget_mw = self.maximum_mw;
        Some(self.budget_mw)
    }

    /// Integral to engage with: the converged one while it is fresh, decaying to the cold-start
    /// integral (a fraction of the ceiling) after a long period without control.
    fn resume_integral(&self, now_ns: u64) -> i64 {
        let initial = self.ceiling_mw * INITIAL_NUM / INITIAL_DEN;
        if self.converged_mw == 0 {
            return initial;
        }
        let kept = self.converged_mw.min(self.ceiling_mw);
        let cool_ns = now_ns.saturating_sub(self.last_active_ns);
        if cool_ns <= MEMORY_HOLD_NS {
            return kept;
        }
        let decay_ns = (cool_ns - MEMORY_HOLD_NS).min(MEMORY_DECAY_NS);
        kept + (initial - kept) * (decay_ns / 1_000_000) as i64 / (MEMORY_DECAY_NS / 1_000_000) as i64
    }

    /// Learn from the firmware's emergency clamp: lower the effective target by a step on each
    /// clamp seen while governing, raise it again after a long period without one.
    fn adapt_target(&mut self, clamped: bool, now_ns: u64) {
        if clamped && !self.was_clamped && self.active {
            let limit = (target_mc() - CLAMP_TARGET_FLOOR_MC).clamp(0, CLAMP_TARGET_MAX_MC);
            self.target_offset_mc = (self.target_offset_mc + CLAMP_TARGET_STEP_MC).min(limit);
            self.target_offset_ns = now_ns;
        } else if !clamped && self.target_offset_mc > 0
            && now_ns.saturating_sub(self.target_offset_ns) >= CLAMP_TARGET_RECOVER_NS
        {
            self.target_offset_mc -= CLAMP_TARGET_STEP_MC;
            self.target_offset_ns = now_ns;
        }
        self.was_clamped = clamped;
    }

    /// One control step with the hotspot temperature, the GPU's current draw (mW) and whether the
    /// firmware's emergency clamp is active. Returns the budget to publish (if it changed) and
    /// whether to run again.
    pub(crate) fn tick(&mut self, temp_mc: Option<i32>, power_mw: u32, clamped: bool, now_ns: u64)
        -> (Option<u32>, bool)
    {
        let maximum = i64::from(self.maximum_mw);
        let idle = now_ns.saturating_sub(self.last_activity_ns) >= IDLE_RESTORE_NS;
        let dt_ns = now_ns.saturating_sub(self.last_tick_ns).min(2 * IDLE_RESTORE_NS);
        self.last_tick_ns = now_ns;
        let previous_mc = core::mem::replace(&mut self.last_temp_mc, temp_mc);
        if enabled() {
            self.adapt_target(clamped, now_ns);
        }
        let target = target_mc() - self.target_offset_mc;
        let desired = match temp_mc {
            _ if maximum == 0 => return (None, false),
            None => {
                self.active = false;
                maximum
            }
            _ if idle || !enabled() => {
                self.active = false;
                maximum
            }
            Some(temp) => {
                let error_mc = i64::from(target - temp);
                let proportional = KP_MW_PER_C * error_mc / 1000;
                let rise_mc_per_s = match previous_mc {
                    Some(previous) if temp > previous && dt_ns >= 1_000_000 =>
                        i64::from(temp - previous) * 1000 / (dt_ns / 1_000_000) as i64,
                    _ => 0,
                };
                let feed_forward = KF_MW_PER_C_PER_S * rise_mc_per_s / 1000;
                if !self.active && temp >= target - ENGAGE_BELOW_TARGET_MC {
                    self.active = true;
                    self.ceiling_mw = i64::from(power_mw).max(CEILING_MIN_MW).clamp(FLOOR_MW, maximum);
                    self.integral_mw = self.resume_integral(now_ns);
                }
                if self.active && clamped {
                    // The firmware's emergency clamp: hold the budget and the integral until it
                    // ends, so the GPU resumes at the converged budget.
                    self.last_active_ns = now_ns;
                    i64::from(self.budget_mw)
                } else if self.active {
                    self.last_active_ns = now_ns;
                    let floor = FLOOR_MW.min(self.ceiling_mw);
                    let candidate = self.integral_mw + KI_MW_PER_C_S * error_mc * (dt_ns / 1_000_000) as i64 / 1_000_000;
                    let output = proportional + candidate - feed_forward;
                    // Anti-windup: integrate only while the output is inside its range; at a
                    // limit, back-calculate the integral so the output sits on the limit.
                    let output = if output > self.ceiling_mw {
                        self.integral_mw = self.ceiling_mw - proportional + feed_forward;
                        self.ceiling_mw
                    } else if output < floor {
                        self.integral_mw = floor - proportional + feed_forward;
                        floor
                    } else {
                        self.integral_mw = candidate;
                        self.converged_mw = candidate;
                        output
                    };
                    if output >= self.ceiling_mw
                        && temp < target - ENGAGE_BELOW_TARGET_MC - RELEASE_HYSTERESIS_MC
                    {
                        // Released: back to the calibrated maximum.
                        self.active = false;
                        maximum
                    } else {
                        output
                    }
                } else {
                    maximum
                }
            }
        };
        let desired = if desired >= maximum { maximum } else { desired - desired % STEP_MW };
        let publish = (desired != i64::from(self.budget_mw)).then(|| {
            self.budget_mw = desired as u32;
            self.budget_mw
        });
        let again = (!idle && requested()) || self.budget_mw != self.maximum_mw;
        (publish, again)
    }
}

// SPDX-License-Identifier: GPL-2.0-only OR MIT
#![recursion_limit = "2048"]

//! Apple T8140 (J700, MacBook Neo) AOP audio
//!
//! The AOP firmware owns the audio fabric on this SoC, so the paths the AP
//! sees are its services:
//!
//! * `lpai`, the low-power microphone: 16 kHz stereo S32 frames (24
//!   meaningful bits) produced into a 512 KiB host ring mapped through
//!   dart-aop stream 9, with a cursor report every 3,200 frames.  There is
//!   no DMA engine on the host side: the ring is bound to the firmware over
//!   the AOP setup port (the AOP driver owns it, the firmware binds it once
//!   per boot) and the driver copies each reported span into the ALSA
//!   buffer of the NeoMic card.  This follows the native m1n1 host that
//!   proved the path (m1n1-aurora proxyclient/m1n1/fw/aop/lpmic_probe.py).
//! * `cout` (the CS42L83 jack on MCA1, fed by base-ns ADMAC TX2) and `spkr`
//!   (the MAX98360 speaker on LEAP TX0, fed by leap-ns ADMAC TX0): ASoC
//!   back-end DAIs that attach the service, request its power states
//!   (`pw0 ` idle, `pwrd` running, the firmware then runs the serializer
//!   clocks) and power the AP-side PMGR leaves, with DPCM front-end PCMs on
//!   the ADMAC channels.  The macaudio J700 variant links them to the
//!   codecs.  The speaker path additionally carries the bench proofs
//!   (zero-DMA, bounded tone) used to establish its sample format.
//!
//! Copyright (C) The Asahi Linux Contributors

use core::{
    arch::asm,
    mem,
    ptr,
    slice, //
};

use kernel::{
    bindings,
    c_str,
    device,
    device::Core,
    dma::{
        Coherent,
        Device as DmaDevice,
        DmaMask, //
    },
    module_platform_driver,
    new_spinlock,
    of,
    platform,
    prelude::*,
    soc::apple::aop::{
        from_fourcc,
        EPICService,
        ReportListener,
        SourceRing,
        AOP, //
    },
    sync::{
        aref::ARef,
        atomic::{
            Atomic,
            Relaxed, //
        },
        Arc,
        SpinLock, //
    },
    time::{
        hrtimer::{
            HrTimer,
            HrTimerCallback,
            HrTimerCallbackContext,
            HrTimerRestart,
            RelativeMode,
            ScopedHrTimerPointer, //
        },
        Delta,
        Instant,
        Monotonic, //
    },
    types::{ForeignOwnable, Opaque, ScopeGuard},
    workqueue::{
        self,
        DelayedWork,
        WorkItem, //
    }, //
};
use kernel::impl_has_hr_timer;
use kernel::time::msecs_to_jiffies;
use kernel::workqueue::{impl_has_delayed_work, impl_has_work, new_delayed_work, new_work, Work};
use pin_init::stack_pin_init;

const EPIC_SUBTYPE_WRAPPED_CALL: u16 = 0x20;
const EPIC_SUBTYPE_PRODUCER_REPORT: u16 = 0x20;
const CALLTYPE_AUDIO_ATTACH_DEVICE: u32 = 0xc3000002;
const CALLTYPE_AUDIO_GET_PROP: u32 = 0xc3000004;
const CALLTYPE_AUDIO_SET_PROP: u32 = 0xc3000005;
const AUDIO_DEV_LPAI: u32 = from_fourcc(b"lpai");
const PROP_POWER_STATE: u32 = 200;
const PROP_POWER_REQUEST: u32 = 202;
const PROP_CHANNEL_CONTROL: u32 = 300;
const PROP_STREAM_FORMAT: u32 = 301;
const PROP_RING_GEOMETRY: u32 = 302;
const POWER_STATE_RUN: u32 = from_fourcc(b"runn");
const POWER_STATE_IDLE: u32 = from_fourcc(b"idle");

const AUDIO_DEV_SPKR: u32 = from_fourcc(b"spkr");
const AUDIO_DEV_TAP: u32 = from_fourcc(b"tap ");
const AUDIO_DEV_COUT: u32 = from_fourcc(b"cout");
const AUDIO_DEV_CIN: u32 = from_fourcc(b"cin ");
/// kIOReturnBusy from the firmware: the device is already attached.
const AUDIO_RET_BUSY: u32 = 0xe00002d5;
const SPKR_POWER_STATE_PW0: u32 = from_fourcc(b"pw0 ");
const SPKR_POWER_STATE_PWRD: u32 = from_fourcc(b"pwrd");
const SPKR_PROP_401: u32 = 401;
const SPKR_PROP_402: u32 = 402;
const SPKR_PD_TX0: u32 = 0;
const SPKR_PD_MCA: u32 = 1;
const SPKR_TEST_BUF_BYTES: usize = 0x4000;
const SPKR_TEST_PERIOD_BYTES: usize = 0x1000;
const SPKR_TEST_REPORTS: u32 = 4;
/// Bounded tone (bench): 48 kHz, S32 stereo, 1 kHz.  The buffer holds
/// 40 ms of silence, the tone, and silence to the end; four 32 KiB
/// descriptors as in the native proof.
const SPKR_TONE_RATE: u32 = 48000;
const SPKR_TONE_BUF_BYTES: usize = 0x20000;
const SPKR_TONE_PERIOD_BYTES: usize = 0x8000;
const SPKR_TONE_LEAD_FRAMES: usize = 48000 * 40 / 1000;
const SPKR_TONE_RAMP_FRAMES: usize = 48000 * 20 / 1000;
const SPKR_TONE_MS_MAX: u32 = 150;
/// Numeric input bound for diagnostics (-50 dB relative to unity).
/// This is not an acoustic or electrical safety limit until LEAP gain is known.
const SPKR_TONE_AMP_MAX: u32 = 6_791_000;
/// Amplifier window: the thread lowers GPIO105 after `spkr_amp_ms` (at
/// most SPKR_AMP_MS_MAX) and an hrtimer forces it low at SPKR_AMP_GUARD_MS
/// no matter what the thread does.
const SPKR_AMP_MS_MAX: u32 = 280;
const SPKR_AMP_GUARD_MS: i64 = 300;
/// sin(2*pi*k/48) * 32767, one 1 kHz cycle at 48 kHz.
const SINE48: [i32; 48] = [
    0, 4277, 8481, 12539, 16383, 19947, 23170, 25996, 28377, 30273, 31650, 32487,
    32767, 32487, 31650, 30273, 28377, 25996, 23170, 19947, 16383, 12539, 8481, 4277,
    0, -4277, -8481, -12539, -16383, -19947, -23170, -25996, -28377, -30273, -31650, -32487,
    -32767, -32487, -31650, -30273, -28377, -25996, -23170, -19947, -16383, -12539, -8481, -4277,
];

const RATE: u32 = 16000;
const CHANNELS: u32 = 2;
const FRAME_BYTES: usize = 8;
const RING_BYTES: usize = 0x80000;
const PERIOD_FRAMES: usize = 3200;
const PERIOD_BYTES: usize = PERIOD_FRAMES * FRAME_BYTES;
const PERIODS_MAX: u32 = 16;
const REPORT_MIN_LEN: usize = 0x68;

fn mem_sync() {
    // SAFETY: a full barrier so the tone is visible to the DMA engine.
    unsafe {
        asm!("dsb sy");
    }
}

// Firmware power timestamps use the 24 MHz always-on timebase. The
// architectural counter can run at a different rate (1 GHz on T8140).
const AOP_TIMEBASE_HZ: u64 = 24_000_000;

fn aop_counter_ticks(counter: u64, frequency: u64) -> Result<u64> {
    if frequency == 0 || frequency > u32::MAX as u64 {
        return Err(EINVAL);
    }
    // Split whole seconds from the remainder to avoid overflowing a u64
    // multiplication on an otherwise ordinary long-running system.
    let seconds = counter / frequency;
    let remainder = (counter % frequency) * AOP_TIMEBASE_HZ / frequency;
    seconds.checked_mul(AOP_TIMEBASE_HZ)
        .and_then(|ticks| ticks.checked_add(remainder))
        .ok_or(EOVERFLOW)
}

fn aop_timestamp() -> Result<u64> {
    let counter: u64;
    let frequency: u64;
    // SAFETY: architectural read-only timer registers; no private EL2
    // register access is needed for the firmware's timestamp domain.
    unsafe {
        asm!("isb", "mrs {counter}, cntvct_el0", "mrs {frequency}, cntfrq_el0",
             counter = out(reg) counter, frequency = out(reg) frequency,
             options(nostack, preserves_flags));
    }
    aop_counter_ticks(counter, frequency)
}

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct AudioAttachDevice {
    _zero0: u32,
    unk0: u32,
    calltype: u32,
    _zero1: [u8; 16],
    cookie: u32,
    len: u64,
    dev_id: u32,
    _zero2: u32,
}

impl AudioAttachDevice {
    fn new(dev_id: u32) -> AudioAttachDevice {
        AudioAttachDevice {
            unk0: 0xffffffff,
            calltype: CALLTYPE_AUDIO_ATTACH_DEVICE,
            len: 0x2c,
            dev_id,
            ..AudioAttachDevice::default()
        }
    }
}

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct AudioGetDeviceProp {
    _zero0: u32,
    unk0: u32,
    calltype: u32,
    _zero1: [u8; 16],
    _pad0: u32,
    len: u64,
    dev_id: u32,
    modifier: u32,
    unk6: u32,
}

impl AudioGetDeviceProp {
    fn new(dev_id: u32, modifier: u32) -> AudioGetDeviceProp {
        AudioGetDeviceProp {
            unk0: 0xffffffff,
            calltype: CALLTYPE_AUDIO_GET_PROP,
            len: 0x30,
            dev_id,
            modifier,
            unk6: 1,
            ..AudioGetDeviceProp::default()
        }
    }
}

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct AudioSetDeviceProp<T: Copy + Default> {
    _zero0: u32,
    unk0: u32,
    calltype: u32,
    _zero1: [u8; 16],
    _pad0: u32,
    len: u64,
    dev_id: u32,
    modifier: u32,
    len2: u32,
    data: T,
}

impl<T: Copy + Default> AudioSetDeviceProp<T> {
    fn new(dev_id: u32, modifier: u32, data: T) -> AudioSetDeviceProp<T> {
        AudioSetDeviceProp {
            unk0: 0xffffffff,
            calltype: CALLTYPE_AUDIO_SET_PROP,
            len: mem::size_of::<T>() as u64 + 0x30,
            dev_id,
            modifier,
            len2: mem::size_of::<T>() as u32,
            data,
            ..AudioSetDeviceProp::default()
        }
    }
}

/// Property 300: supported / enabled / history channel masks.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct ChannelControl {
    supported: u32,
    enabled: u32,
    history: u32,
}

/// Property 202 on t8140: node id, request sequence, CNTVCT timestamp, the
/// target state FourCC and a parameter of 1.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct PowerRequest {
    node: u32,
    sequence: u32,
    timestamp: u64,
    _zero0: u32,
    state: u32,
    param: u32,
    _zero1: [u8; 20],
}

/// Property 202 on the spkr service: FourCC, sequence, 24 MHz timestamp, a
/// zero word, the target state FourCC, a zero parameter and 20 zero bytes.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct SpkrPowerRequest {
    dev_id: u32,
    sequence: u32,
    timestamp: u64,
    _zero0: u32,
    state: u32,
    param: u32,
    _zero1: [u8; 20],
}

struct StreamState {
    substream: *mut bindings::snd_pcm_substream,
    running: bool,
    /// absolute producer byte count at the last report
    last_absolute: Option<u64>,
    /// absolute producer byte count already copied into the ALSA buffer
    copied: Option<u64>,
    hw_ptr_bytes: usize,
    period_acc: usize,
    buffer_bytes: usize,
    overruns: u32,
}

unsafe impl Send for StreamState {}

#[pin_data]
struct LpmicData {
    dev: ARef<device::Device>,
    adata: Arc<dyn AOP>,
    service: EPICService,
    ring: Arc<SourceRing>,
    ring_bytes: Atomic<u32>,
    sequence: Atomic<u32>,
    powered: Atomic<u32>,
    hw_ptr_frames: Atomic<u32>,
    spkr_sequence: Atomic<u32>,
    spkr_reports: Atomic<u32>,
    tap_sequence: Atomic<u32>,
    /// The tap service is in pwrd (sense back-end prepared).
    tap_running: Atomic<u32>,
    /// audio_mca0_m (the tap's serializer domain), 0 if absent.
    pd_mca0: Atomic<usize>,
    cout_sequence: Atomic<u32>,
    cout_running: Atomic<u32>,
    cin_sequence: Atomic<u32>,
    cin_running: Atomic<u32>,
    /// audio_mca1_m virtual power-domain device (dev_pm_domain_attach_by_name), 0 if none.
    pd_mca1: Atomic<usize>,
    /// The speaker path's LEAP domains (audio_leap_tx0, audio_leap_mca):
    /// virtual power-domain devices held for the driver's lifetime and
    /// runtime-powered around speaker streams; 0 if absent.
    pd_tx0: Atomic<usize>,
    pd_leap_mca: Atomic<usize>,
    /// The spkr service is in pwrd and unmuted (speaker back-end prepared).
    spkr_running: Atomic<u32>,
    /// A speaker stream has been triggered and not yet stopped.
    spkr_want_run: Atomic<u32>,
    /// Copy calls into the speaker buffer since the last prepare (debug).
    spkr_copies: Atomic<u32>,
    /// "Speaker Playback Volume" (REG_SPKR_VOLUME), 0..=SPKR_VOLUME_MAX.
    spkr_volume: Atomic<u32>,
    /// DMA channels of the front-end PCMs, [FE DAI id][stream] (raw dma_chan pointers).
    #[pin]
    fe_chans: SpinLock<[[usize; 2]; FE_COUNT]>,
    #[pin]
    stream: SpinLock<StreamState>,
}

unsafe impl Send for LpmicData {}
unsafe impl Sync for LpmicData {}

impl LpmicData {
    fn epic_wrapped_call<T>(&self, data: &T) -> Result<u32> {
        // SAFETY: T is a plain packed firmware record.
        let msg_bytes =
            unsafe { slice::from_raw_parts(data as *const T as *const u8, mem::size_of::<T>()) };
        self.adata
            .epic_call(&self.service, EPIC_SUBTYPE_WRAPPED_CALL, msg_bytes)
    }

    fn epic_wrapped_call_ret<T>(&self, data: &T, ret_len: usize) -> Result<KVec<u8>> {
        // SAFETY: T is a plain packed firmware record.
        let msg_bytes =
            unsafe { slice::from_raw_parts(data as *const T as *const u8, mem::size_of::<T>()) };
        let (retcode, ret) =
            self.adata
                .epic_call_ret(&self.service, EPIC_SUBTYPE_WRAPPED_CALL, msg_bytes, ret_len)?;
        if retcode != 0 {
            dev_err!(self.dev, "lpai call failed, return code {:#x}\n", retcode);
            return Err(EIO);
        }
        Ok(ret)
    }

    /// Attach an audio device.  The firmware keeps attachments for its
    /// lifetime (there is no detach in the protocol we know), so a reload of
    /// this module sees kIOReturnBusy for a device it attached earlier.
    fn attach_device(&self, dev_id: u32, name: &str) -> Result<()> {
        let ret = self.epic_wrapped_call(&AudioAttachDevice::new(dev_id))?;
        match ret {
            0 => Ok(()),
            AUDIO_RET_BUSY => {
                dev_info!(self.dev, "{} already attached by an earlier instance\n", name);
                Ok(())
            }
            _ => {
                dev_err!(self.dev, "unable to attach {}, return code {:#x}\n", name, ret);
                Err(EIO)
            }
        }
    }

    fn attach_lpai(&self) -> Result<()> {
        self.attach_device(AUDIO_DEV_LPAI, "lpai")
    }

    fn set_channel_control(&self) -> Result<()> {
        let ctl = ChannelControl {
            supported: 3,
            enabled: 3,
            history: 3,
        };
        let ret = self.epic_wrapped_call(&AudioSetDeviceProp::new(
            AUDIO_DEV_LPAI,
            PROP_CHANNEL_CONTROL,
            ctl,
        ))?;
        if ret != 0 {
            dev_err!(self.dev, "unable to set lpai channels, return code {:#x}\n", ret);
            return Err(EIO);
        }
        Ok(())
    }

    /// Reply layout after the retcode: u32 length, then the property bytes.
    fn get_prop(&self, modifier: u32, len: usize) -> Result<KVec<u8>> {
        let ret = self.epic_wrapped_call_ret(&AudioGetDeviceProp::new(AUDIO_DEV_LPAI, modifier), 4 + len)?;
        if ret.len() < 4 + len {
            dev_err!(self.dev, "lpai property {} reply too short ({})\n", modifier, ret.len());
            return Err(EIO);
        }
        let reported = u32::from_le_bytes(ret[..4].try_into().unwrap()) as usize;
        if reported != len {
            dev_err!(self.dev, "lpai property {} length {} (expected {})\n", modifier, reported, len);
            return Err(EIO);
        }
        Ok(ret)
    }

    fn read_geometry(&self) -> Result<()> {
        // 302: FourCC/pad, ring frames u64, report frames u64
        let geo = self.get_prop(PROP_RING_GEOMETRY, 24)?;
        let frames = u64::from_le_bytes(geo[12..20].try_into().unwrap());
        let quantum = u64::from_le_bytes(geo[20..28].try_into().unwrap());
        if frames == 0 || frames as usize * FRAME_BYTES > RING_BYTES || quantum == 0 {
            dev_err!(self.dev, "invalid lpai ring geometry: {} frames, {} per report\n", frames, quantum);
            return Err(EIO);
        }
        self.ring_bytes.store((frames as usize * FRAME_BYTES) as u32, Relaxed);
        // 301: stream format, sample rate at +4
        let fmt = self.get_prop(PROP_STREAM_FORMAT, 16)?;
        let rate = u32::from_le_bytes(fmt[8..12].try_into().unwrap());
        if rate != RATE {
            dev_err!(self.dev, "unexpected lpai sample rate {}\n", rate);
            return Err(EIO);
        }
        dev_info!(
            self.dev,
            "lpai: {} Hz, ring {} frames ({} bytes), {} frames per report\n",
            rate,
            frames,
            frames * FRAME_BYTES as u64,
            quantum
        );
        Ok(())
    }

    fn set_power(&self, state: u32) -> Result<()> {
        let sequence = self.sequence.fetch_add(1, Relaxed) + 1;
        let req = PowerRequest {
            node: 0,
            sequence,
            timestamp: aop_timestamp()?,
            state,
            param: 1,
            ..PowerRequest::default()
        };
        let ret = self.epic_wrapped_call(&AudioSetDeviceProp::new(
            AUDIO_DEV_LPAI,
            PROP_POWER_REQUEST,
            req,
        ))?;
        if ret != 0 {
            dev_err!(self.dev, "lpai power request {:#x} rejected: {:#x}\n", state, ret);
            return Err(EIO);
        }
        let st = self.get_prop(PROP_POWER_STATE, 4)?;
        let now = u32::from_le_bytes(st[4..8].try_into().unwrap());
        if now != state {
            dev_err!(self.dev, "lpai power state {:#x} after requesting {:#x}\n", now, state);
            return Err(EIO);
        }
        Ok(())
    }

    // ---- internal speaker: LEAP TX0 through admac-leap-ns, AOP spkr service ----

    /// Bench: dump a device property reply (length word + payload) without
    /// assuming its size, for the spkr/cout format descriptors.
    fn dump_prop(&self, dev_id: u32, name: &str, modifier: u32) {
        match self.epic_wrapped_call_ret(&AudioGetDeviceProp::new(dev_id, modifier), 4 + 0x40) {
            Ok(ret) => {
                let reported = u32::from_le_bytes(ret[..4].try_into().unwrap()) as usize;
                let n = core::cmp::min(reported, 0x40);
                let mut hex = [0u8; 0x40 * 3];
                let mut k = 0;
                for b in &ret[4..4 + n] {
                    let d = b"0123456789abcdef";
                    hex[k] = d[(b >> 4) as usize];
                    hex[k + 1] = d[(b & 15) as usize];
                    hex[k + 2] = b' ';
                    k += 3;
                }
                dev_info!(self.dev, "{} property {}: {} bytes: {}\n", name, modifier, reported,
                          core::str::from_utf8(&hex[..k]).unwrap_or("?"));
            }
            Err(e) => dev_info!(self.dev, "{} property {}: {:?}\n", name, modifier, e),
        }
    }

    fn spkr_get_prop(&self, modifier: u32, len: usize) -> Result<KVec<u8>> {
        let ret = self.epic_wrapped_call_ret(&AudioGetDeviceProp::new(AUDIO_DEV_SPKR, modifier), 4 + len)?;
        if ret.len() < 4 + len || u32::from_le_bytes(ret[..4].try_into().unwrap()) as usize != len {
            dev_err!(self.dev, "spkr property {} reply malformed ({} bytes)\n", modifier, ret.len());
            return Err(EIO);
        }
        Ok(ret)
    }

    fn spkr_set_power(&self, state: u32) -> Result<()> {
        self.out_set_power(AUDIO_DEV_SPKR, "spkr", &self.spkr_sequence, state)
    }

    // Output mute/ramp is independent of the amplifier enable GPIO. Keep the
    // amplifier disabled while changing this state or handling an IPC failure.
    fn spkr_set_mute(&self, muted: bool) -> Result<()> {
        let ret = self.epic_wrapped_call(&AudioSetDeviceProp::new(AUDIO_DEV_SPKR, 700, muted as u32))?;
        if ret != 0 {
            dev_err!(self.dev, "speaker mute request rejected: {:#x}\n", ret);
            return Err(EIO);
        }
        Ok(())
    }

    /// Output device (spkr/cout) power request: property 202 with a per-
    /// device sequence and a 24 MHz timestamp; pw0 is confirmed through
    /// property 200 like the native host did.
    fn out_set_power(&self, dev_id: u32, name: &str, seq: &Atomic<u32>, state: u32) -> Result<()> {
        let sequence = seq.fetch_add(1, Relaxed) + 1;
        let req = SpkrPowerRequest {
            dev_id,
            sequence,
            timestamp: aop_timestamp()?,
            state,
            param: 0,
            ..SpkrPowerRequest::default()
        };
        let ret = self.epic_wrapped_call(&AudioSetDeviceProp::new(dev_id, PROP_POWER_REQUEST, req))?;
        if ret != 0 {
            dev_err!(self.dev, "{} power request {:#x} rejected: {:#x}\n", name, state, ret);
            return Err(EIO);
        }
        if state == SPKR_POWER_STATE_PW0 {
            let ret = self.epic_wrapped_call_ret(&AudioGetDeviceProp::new(dev_id, PROP_POWER_STATE), 8)?;
            if ret.len() < 8 {
                return Err(EIO);
            }
            let now = u32::from_le_bytes(ret[4..8].try_into().unwrap());
            if now != state {
                dev_err!(self.dev, "{} power state {:#x} after requesting {:#x}\n", name, now, state);
                return Err(EIO);
            }
        }
        Ok(())
    }

    // ---- speaker sense: the native SpeakerTap, base-ns RX1 under the tap service on MCA0 ----
    //
    // The words the amplifier receives come back on this stream (the card's
    // "Speaker Sense" capture, device 2); it observes the serial data, it is
    // not a current or voltage sensor, and it never touches the amplifier.

    /// Sense back-end startup: attach the tap service, power MCA0, pw0.
    fn sense_startup(&self) -> Result<()> {
        self.attach_device(AUDIO_DEV_TAP, "tap")?;
        let pd = self.pd_mca0.load(Relaxed);
        if pd != 0 {
            let rc = unsafe { bindings::pm_runtime_get_sync(pd as *mut bindings::device) };
            if rc < 0 {
                dev_err!(self.dev, "tap: audio_mca0_m did not power up: {}\n", rc);
                return Err(EIO);
            }
        }
        if let Err(e) = self.out_set_power(AUDIO_DEV_TAP, "tap", &self.tap_sequence, SPKR_POWER_STATE_PW0) {
            if pd != 0 {
                unsafe { bindings::pm_runtime_put_sync(pd as *mut bindings::device) };
            }
            return Err(e);
        }
        self.tap_running.store(0, Relaxed);
        Ok(())
    }

    /// Sense back-end prepare: pwrd starts the tap delivering into the RX DMA.
    fn sense_prepare(&self) -> Result<()> {
        if self.tap_running.load(Relaxed) != 0 {
            return Ok(());
        }
        self.out_set_power(AUDIO_DEV_TAP, "tap", &self.tap_sequence, SPKR_POWER_STATE_PWRD)?;
        self.tap_running.store(1, Relaxed);
        Ok(())
    }

    /// Sense back-end shutdown: pw0, release MCA0.
    fn sense_shutdown(&self) {
        if self.tap_running.load(Relaxed) != 0 {
            if let Err(e) = self.out_set_power(AUDIO_DEV_TAP, "tap", &self.tap_sequence, SPKR_POWER_STATE_PW0) {
                dev_warn!(self.dev, "tap: pw0 failed: {:?}\n", e);
            }
            self.tap_running.store(0, Relaxed);
        }
        let pd = self.pd_mca0.load(Relaxed);
        if pd != 0 {
            unsafe { bindings::pm_runtime_put_sync(pd as *mut bindings::device) };
        }
    }

    // ---- 3.5 mm jack: the AOP cout/cin services on MCA1, base-ns TX2/RX2 ----

    /// The jack service for a stream direction: cout (playback) or cin
    /// (the headset microphone, capture).
    fn jack_service(playback: bool) -> (u32, &'static str) {
        if playback {
            (AUDIO_DEV_COUT, "cout")
        } else {
            (AUDIO_DEV_CIN, "cin")
        }
    }

    fn jack_state(&self, playback: bool) -> (&Atomic<u32>, &Atomic<u32>) {
        if playback {
            (&self.cout_sequence, &self.cout_running)
        } else {
            (&self.cin_sequence, &self.cin_running)
        }
    }

    /// Back-end DAI startup: attach the service, power audio_mca1_m (held
    /// once per open direction), pw0.
    fn jack_startup(&self, playback: bool) -> Result<()> {
        let (dev_id, name) = Self::jack_service(playback);
        let (seq, running) = self.jack_state(playback);
        self.attach_device(dev_id, name)?;
        let pd = self.pd_mca1.load(Relaxed);
        if pd != 0 {
            let rc = unsafe { bindings::pm_runtime_get_sync(pd as *mut bindings::device) };
            if rc < 0 {
                dev_err!(self.dev, "{}: audio_mca1_m did not power up: {}\n", name, rc);
                return Err(EIO);
            }
        }
        self.out_set_power(dev_id, name, seq, SPKR_POWER_STATE_PW0)?;
        running.store(0, Relaxed);
        dev_info!(self.dev, "{}: attached, mca1 powered, pw0\n", name);
        Ok(())
    }

    /// Back-end DAI prepare: pwrd starts the MCA1 clocks (the codec's PLL
    /// locks to them when ASoC unmutes it right after this).
    fn jack_prepare(&self, playback: bool) -> Result<()> {
        let (dev_id, name) = Self::jack_service(playback);
        let (seq, running) = self.jack_state(playback);
        if running.load(Relaxed) != 0 {
            return Ok(());
        }
        self.out_set_power(dev_id, name, seq, SPKR_POWER_STATE_PWRD)?;
        running.store(1, Relaxed);
        dev_info!(self.dev, "{}: pwrd (MCA1 clocks running)\n", name);
        Ok(())
    }

    /// Back-end DAI shutdown: pw0 (the codec is already muted and back on
    /// its oscillator), then the power domain.
    fn jack_shutdown(&self, playback: bool) {
        let (dev_id, name) = Self::jack_service(playback);
        let (seq, running) = self.jack_state(playback);
        if running.load(Relaxed) != 0 {
            if let Err(e) = self.out_set_power(dev_id, name, seq, SPKR_POWER_STATE_PW0) {
                dev_err!(self.dev, "{}: pw0 failed: {:?}\n", name, e);
            }
            running.store(0, Relaxed);
        }
        let pd = self.pd_mca1.load(Relaxed);
        if pd != 0 {
            unsafe { bindings::pm_runtime_put_sync(pd as *mut bindings::device) };
        }
        dev_info!(self.dev, "{}: pw0, mca1 released\n", name);
    }

    // ---- internal speaker: the AOP spkr service on LEAP TX0 (admac-leap-ns 0) ----

    /// Runtime-power (or release) the LEAP leaves of the speaker path.
    fn spkr_domains(&self, on: bool) -> Result<()> {
        let pds = [self.pd_tx0.load(Relaxed), self.pd_leap_mca.load(Relaxed)];
        if pds.iter().any(|&pd| pd == 0) {
            dev_err!(self.dev, "spkr: LEAP power domains are not attached\n");
            return Err(ENODEV);
        }
        if on {
            for (i, &pd) in pds.iter().enumerate() {
                let rc = unsafe { bindings::pm_runtime_get_sync(pd as *mut bindings::device) };
                if rc < 0 {
                    dev_err!(self.dev, "spkr: LEAP power domain {} did not power up: {}\n", i, rc);
                    for &done in pds[..=i].iter() {
                        unsafe { bindings::pm_runtime_put_sync(done as *mut bindings::device) };
                    }
                    return Err(EIO);
                }
            }
        } else {
            for &pd in pds.iter().rev() {
                unsafe { bindings::pm_runtime_put_sync(pd as *mut bindings::device) };
            }
        }
        Ok(())
    }

    /// Speaker back-end startup: attach the service, power the LEAP leaves,
    /// pw0 -- the order the native host proved (zero-DMA, then the tone).
    fn spkr_startup(&self) -> Result<()> {
        self.attach_device(AUDIO_DEV_SPKR, "spkr")?;
        self.spkr_domains(true)?;
        if let Err(e) = self.spkr_set_power(SPKR_POWER_STATE_PW0) {
            let _ = self.spkr_domains(false);
            return Err(e);
        }
        self.spkr_running.store(0, Relaxed);
        dev_info!(self.dev, "spkr: attached, LEAP domains powered, pw0\n");
        Ok(())
    }

    /// Speaker stream start, from the system workqueue right after trigger
    /// START (the DMA is running by then, the order the native host and the
    /// bench proved: descriptors first, then pwrd): pwrd starts the LEAP
    /// consuming, then the firmware unmute (property 700, the value the
    /// native amplifier driver writes after enabling its output).
    fn spkr_go(&self) -> Result<()> {
        if self.spkr_want_run.load(Relaxed) == 0 || self.spkr_running.load(Relaxed) != 0 {
            return Ok(());
        }
        self.spkr_set_power(SPKR_POWER_STATE_PWRD)?;
        if let Err(e) = self.spkr_set_mute(false) {
            let _ = self.spkr_set_power(SPKR_POWER_STATE_PW0);
            return Err(e);
        }
        self.spkr_running.store(1, Relaxed);
        dev_info!(self.dev, "spkr: pwrd, unmuted (wire ceiling -20 dBFS), {} copies so far\n",
                  self.spkr_copies.load(Relaxed));
        Ok(())
    }

    /// Speaker back-end shutdown: mute, pw0, release the LEAP leaves.  The
    /// amplifier is already off (codec trigger STOP); a start still queued
    /// was flushed by the caller.
    fn spkr_shutdown(&self) {
        self.spkr_want_run.store(0, Relaxed);
        if self.spkr_running.load(Relaxed) != 0 {
            if let Err(e) = self.spkr_set_mute(true) {
                dev_err!(self.dev, "spkr: mute failed: {:?}\n", e);
            }
            if let Err(e) = self.spkr_set_power(SPKR_POWER_STATE_PW0) {
                dev_err!(self.dev, "spkr: pw0 failed: {:?}\n", e);
            }
            self.spkr_running.store(0, Relaxed);
        }
        let _ = self.spkr_domains(false);
        dev_info!(self.dev, "spkr: muted, pw0, LEAP domains released\n");
    }

    /// The native host's zero-DMA proof: attach spkr, read 401/402, pw0,
    /// power the LEAP domains parent-first, run four 4 KiB silence
    /// descriptors on LEAP TX0 with the stream in pwrd, back to pw0.  The
    /// amplifier enable (GPIO105) is never touched.
    fn speaker_zero_dma_test(&self, wave: SpkrWave) -> Result<()> {
        self.attach_device(AUDIO_DEV_SPKR, "spkr")?;
        let p401 = self.spkr_get_prop(SPKR_PROP_401, 4)?;
        let p402 = self.spkr_get_prop(SPKR_PROP_402, 4)?;
        dev_info!(
            self.dev,
            "spkr attached: property 401 {:#x}, 402 {:#x}\n",
            u32::from_le_bytes(p401[4..8].try_into().unwrap()),
            u32::from_le_bytes(p402[4..8].try_into().unwrap())
        );
        self.spkr_set_power(SPKR_POWER_STATE_PW0)?;
        dev_info!(self.dev, "spkr: pw0 confirmed\n");

        // LEAP domains: tx0 (-> leap_c -> leap_a -> audio_fr -> audio_a) and
        // leap_mca (-> audio_p -> audio_fr), attached by index and powered up.
        let raw_dev = self.dev.as_raw();
        let mut pds: [*mut bindings::device; 2] = [ptr::null_mut(); 2];
        let result = (|| -> Result<()> {
            for (i, idx) in [SPKR_PD_TX0, SPKR_PD_MCA].iter().enumerate() {
                // SAFETY: plain C calls on our bound device.
                let pd = unsafe { bindings::dev_pm_domain_attach_by_id(raw_dev, *idx) };
                if pd.is_null() {
                    dev_err!(self.dev, "no power domain {} for the speaker\n", idx);
                    return Err(ENODEV);
                }
                let err = kernel::error::from_err_ptr(pd);
                let pd = match err {
                    Ok(p) => p,
                    Err(e) => {
                        dev_err!(self.dev, "power domain {} attach failed: {:?}\n", idx, e);
                        return Err(e);
                    }
                };
                let rc = unsafe { bindings::pm_runtime_get_sync(pd) };
                if rc < 0 {
                    dev_err!(self.dev, "power domain {} did not power up: {}\n", idx, rc);
                    unsafe {
                        bindings::pm_runtime_put_noidle(pd);
                        bindings::dev_pm_domain_detach(pd, true);
                    };
                    return Err(EIO);
                }
                pds[i] = pd;
            }
            dev_info!(self.dev, "spkr: LEAP power domains up\n");

            self.speaker_zero_dma_run(wave)
        })();

        for pd in pds.iter().rev() {
            if !pd.is_null() {
                unsafe {
                    bindings::pm_runtime_put_sync(*pd);
                    bindings::dev_pm_domain_detach(*pd, true);
                }
            }
        }
        result
    }

    /// Fills the DMA buffer: silence, or the ramped 1 kHz tone at `amp`.
    fn spkr_fill(buf: &mut [u8], wave: SpkrWave) {
        buf.fill(0);
        if let SpkrWave::Tone { amp, tone_ms, enc, .. } = wave {
            let tone_frames = (SPKR_TONE_RATE * tone_ms / 1000) as usize;
            let frames = buf.len() / 8;
            for pos in 0..tone_frames {
                let frame = SPKR_TONE_LEAD_FRAMES + pos;
                if frame >= frames {
                    break;
                }
                let env = core::cmp::min(
                    SPKR_TONE_RAMP_FRAMES,
                    core::cmp::min(pos + 1, tone_frames - pos),
                ) as i64;
                let v = (SINE48[pos % 48] as i64 * amp as i64 / 32767) * env
                    / SPKR_TONE_RAMP_FRAMES as i64;
                let bytes = enc.encode(v as i32);
                buf[frame * 8..frame * 8 + 4].copy_from_slice(&bytes);
                buf[frame * 8 + 4..frame * 8 + 8].copy_from_slice(&bytes);
            }
        }
    }

    /// Takes the amplifier enable (sdmode-gpios of the max98360a node) as
    /// output-low; the codec driver is not loaded on the bench.
    fn spkr_amp_gpio(&self) -> Result<*mut bindings::gpio_desc> {
        // SAFETY: plain OF lookups; the node reference is dropped below.
        let np = unsafe {
            bindings::of_find_compatible_node(
                ptr::null_mut(),
                ptr::null(),
                c_str!("maxim,max98360a").as_char_ptr(),
            )
        };
        if np.is_null() {
            dev_err!(self.dev, "spkr: no max98360a node for the amplifier enable\n");
            return Err(ENODEV);
        }
        let desc = unsafe {
            kernel::error::from_err_ptr(bindings::fwnode_gpiod_get_index(
                &mut (*np).fwnode,
                c_str!("sdmode").as_char_ptr(),
                0,
                bindings::gpiod_flags_GPIOD_OUT_LOW,
                c_str!("j700-spkr-bench").as_char_ptr(),
            ))
        };
        #[cfg(CONFIG_OF_DYNAMIC)]
        unsafe {
            bindings::of_node_put(np)
        };
        let desc = desc?;
        if desc.is_null() {
            return Err(ENODEV);
        }
        if unsafe { bindings::gpiod_cansleep(desc) } != 0 {
            unsafe { bindings::gpiod_put(desc) };
            return Err(EINVAL);
        }
        if unsafe { bindings::gpiod_get_value(desc) } != 0 {
            dev_err!(self.dev, "spkr: amplifier enable is not low, refusing\n");
            unsafe { bindings::gpiod_put(desc) };
            return Err(EIO);
        }
        Ok(desc)
    }

    fn speaker_zero_dma_run(&self, wave: SpkrWave) -> Result<()> {
        let (buf_bytes, period_bytes) = match wave {
            SpkrWave::Silence => (SPKR_TEST_BUF_BYTES, SPKR_TEST_PERIOD_BYTES),
            SpkrWave::Tone { .. } => (SPKR_TONE_BUF_BYTES, SPKR_TONE_PERIOD_BYTES),
        };
        let amp_gpio = match wave {
            SpkrWave::Tone { .. } => Some(self.spkr_amp_gpio()?),
            SpkrWave::Silence => None,
        };
        let _amp_release = ScopeGuard::new(|| {
            if let Some(gpio) = amp_gpio {
                unsafe {
                    bindings::gpiod_set_value(gpio, 0);
                    bindings::gpiod_put(gpio);
                }
            }
        });
        let chan = unsafe {
            kernel::error::from_err_ptr(bindings::dma_request_chan(
                self.dev.as_raw(),
                c_str!("spkr").as_char_ptr(),
            ))
        }?;
        let _channel_release = ScopeGuard::new(|| unsafe { bindings::dma_release_channel(chan) });
        let res = (|| -> Result<()> {
            // The buffer must be mapped through the DMA controller's own
            // stream (dart-aop 10), so allocate it on the controller device.
            // SAFETY: the channel holds the controller bound for its lifetime.
            let dma_dev: &device::Device = unsafe { device::Device::from_raw((*(*chan).device).dev) };
            let bound = unsafe { dma_dev.as_bound() };
            let buf = Coherent::<u8>::zeroed_slice(bound, buf_bytes, GFP_KERNEL)?;
            // Stop the amp and synchronize DMA callbacks before freeing the
            // buffer, including failures after DMA submission.
            let _dma_stop = ScopeGuard::new(|| unsafe {
                if let Some(gpio) = amp_gpio {
                    bindings::gpiod_set_value(gpio, 0);
                }
                if let Some(f) = (*(*chan).device).device_terminate_all {
                    f(chan);
                }
                if let Some(f) = (*(*chan).device).device_synchronize {
                    f(chan);
                }
            });
            // SAFETY: the allocation is buf_bytes long and no DMA runs yet.
            Self::spkr_fill(unsafe { buf.as_mut() }, wave);
            mem_sync();
            dev_info!(self.dev, "spkr: {} buffer at iova {:#x} (controller stream)\n",
                      match wave { SpkrWave::Silence => "silence", SpkrWave::Tone { .. } => "tone" },
                      buf.dma_handle() as u64);
            let mut cfg = bindings::dma_slave_config::default();
            cfg.direction = bindings::dma_transfer_direction_DMA_MEM_TO_DEV;
            cfg.dst_addr_width = bindings::dma_slave_buswidth_DMA_SLAVE_BUSWIDTH_4_BYTES;
            cfg.dst_port_window_size = 2;
            let rc = unsafe {
                match (*(*chan).device).device_config {
                    Some(f) => f(chan, &mut cfg),
                    None => ENOSYS.to_errno(),
                }
            };
            if rc < 0 {
                dev_err!(self.dev, "spkr: DMA slave config failed: {}\n", rc);
                return Err(Error::from_errno(rc));
            }
            self.spkr_reports.store(0, Relaxed);
            let desc = unsafe {
                match (*(*chan).device).device_prep_dma_cyclic {
                    Some(f) => f(
                        chan,
                        buf.dma_handle(),
                        buf_bytes,
                        period_bytes,
                        bindings::dma_transfer_direction_DMA_MEM_TO_DEV,
                        bindings::dma_ctrl_flags_DMA_PREP_INTERRUPT as usize,
                    ),
                    None => ptr::null_mut(),
                }
            };
            if desc.is_null() {
                dev_err!(self.dev, "spkr: cyclic DMA prepare failed\n");
                return Err(EIO);
            }
            unsafe {
                (*desc).callback = Some(spkr_dma_callback);
                (*desc).callback_param = self as *const LpmicData as *mut core::ffi::c_void;
                let submit = (*desc).tx_submit.ok_or(EIO)?;
                let cookie = submit(desc);
                if cookie < 0 {
                    dev_err!(self.dev, "spkr: DMA submit failed: {}\n", cookie);
                    return Err(EIO);
                }
                if let Some(f) = (*(*chan).device).device_issue_pending {
                    f(chan);
                }
            }
            // An unsuccessful power request may still have reached firmware.
            let _power_idle = ScopeGuard::new(|| {
                let _ = self.spkr_set_power(SPKR_POWER_STATE_PW0);
            });
            self.spkr_set_power(SPKR_POWER_STATE_PWRD)?;
            let _mute = ScopeGuard::new(|| { let _ = self.spkr_set_mute(true); });
            self.spkr_set_mute(false)?;
            if let (SpkrWave::Tone { amp, tone_ms, amp_ms, enc }, Some(gpio)) = (wave, amp_gpio) {
                // The amplifier window: pin high, sleep, pin low, with the
                // hrtimer forcing it low at SPKR_AMP_GUARD_MS regardless.
                let t0 = Instant::<Monotonic>::now();
                stack_pin_init!(let guard = AmpGuard::new(gpio));
                let high = guard.as_ref().start_scoped(Delta::from_millis(SPKR_AMP_GUARD_MS), || {
                    if amp_ms != 0 && guard.enable() {
                        let high = unsafe { bindings::gpiod_get_value(gpio) } == 1;
                        if high {
                            kernel::time::delay::fsleep(Delta::from_millis(amp_ms as i64));
                        }
                        guard.disable();
                        high
                    } else {
                        false
                    }
                });
                let window_ms = t0.elapsed().as_millis();
                let low = unsafe { bindings::gpiod_get_value(gpio) } == 0;
                dev_info!(self.dev,
                          "J700 SPEAKER_TONE_WINDOW: amp {} enc {} ({} ms tone), enable {} ms, guard fired {}, pin high {}, pin low {}\n",
                          amp, enc as u32, tone_ms, window_ms, guard.fired.load(Relaxed), high, low);
                if !low {
                    unsafe { bindings::gpiod_set_value(gpio, 0) };
                }
            }
            let mut waited = 0;
            while self.spkr_reports.load(Relaxed) < SPKR_TEST_REPORTS && waited < 3000 {
                kernel::time::delay::fsleep(kernel::time::Delta::from_millis(10));
                waited += 10;
            }
            let reports = self.spkr_reports.load(Relaxed);
            if reports < SPKR_TEST_REPORTS {
                dev_err!(self.dev, "J700 SPEAKER_ZERO_DMA_FAILED: {} of {} period reports in {} ms\n",
                         reports, SPKR_TEST_REPORTS, waited);
                return Err(ETIMEDOUT);
            }
            match wave {
                SpkrWave::Silence => dev_info!(
                    self.dev,
                    "J700 SPEAKER_ZERO_DMA_PROVEN: {} period reports in {} ms, amp never enabled\n",
                    reports, waited
                ),
                SpkrWave::Tone { amp, .. } => dev_info!(
                    self.dev,
                    "J700 SPEAKER_TONE_DONE: amp {} ({} period reports in {} ms), amp enable back low\n",
                    amp, reports, waited
                ),
            }
            Ok(())
        })();
        res
    }

    fn ring_slice(&self, off: usize, len: usize) -> &[u8] {
        // SAFETY: the ring is a coherent allocation of RING_BYTES; callers
        // keep off + len within the active ring.
        unsafe { &self.ring.buf.as_ref()[off..off + len] }
    }
}

/// Copy `len` bytes from the source ring (absolute producer offset `from`)
/// into the ALSA buffer at `hw_ptr`, both wrapping.
fn copy_span(data: &LpmicData, dma_area: *mut u8, buffer_bytes: usize, ring_bytes: usize,
             from: u64, len: usize, hw_ptr: usize) -> usize {
    let mut src = (from % ring_bytes as u64) as usize;
    let mut dst = hw_ptr;
    let mut left = len;
    while left > 0 {
        let n = left.min(ring_bytes - src).min(buffer_bytes - dst);
        let s = data.ring_slice(src, n);
        // SAFETY: dst + n <= buffer_bytes, the ALSA buffer stays mapped while
        // the substream is set in the stream state.
        unsafe { ptr::copy_nonoverlapping(s.as_ptr(), dma_area.add(dst), n) };
        src = (src + n) % ring_bytes;
        dst = (dst + n) % buffer_bytes;
        left -= n;
    }
    dst
}

impl ReportListener for LpmicData {
    fn process_report(&self, _subtype: u16, report: &[u8]) -> Result<()> {
        if report.len() < REPORT_MIN_LEN {
            dev_err!(self.dev, "short lpai producer report ({} bytes)\n", report.len());
            return Ok(());
        }
        let frames = u64::from_le_bytes(report[0x40..0x48].try_into().unwrap()) + 1;
        let cursor = u64::from_le_bytes(report[0x60..0x68].try_into().unwrap());
        let ring_bytes = self.ring_bytes.load(Relaxed) as usize;
        let absolute = frames * FRAME_BYTES as u64;
        if ring_bytes == 0 || cursor != absolute % ring_bytes as u64 {
            dev_warn!(self.dev, "inconsistent lpai report: frames {} cursor {:#x}\n", frames, cursor);
            return Ok(());
        }

        let mut elapsed = false;
        let mut substream = ptr::null_mut();
        {
            let mut st = self.stream.lock();
            st.last_absolute = Some(absolute);
            if !st.running || st.substream.is_null() {
                return Ok(());
            }
            let copied = match st.copied {
                Some(c) => c,
                None => {
                    // start at a completed producer position, not at ring zero
                    st.copied = Some(absolute);
                    return Ok(());
                }
            };
            if absolute < copied {
                dev_warn!(self.dev, "lpai producer moved backwards\n");
                st.copied = Some(absolute);
                return Ok(());
            }
            let delta = (absolute - copied) as usize;
            if delta >= ring_bytes {
                st.overruns += 1;
                st.copied = Some(absolute);
                dev_warn!(self.dev, "lpai source overrun ({} bytes)\n", delta);
                return Ok(());
            }
            if delta == 0 {
                return Ok(());
            }
            let runtime = unsafe { (*st.substream).runtime };
            let dma_area = unsafe { (*runtime).dma_area };
            let buffer_bytes = st.buffer_bytes;
            let new_ptr = copy_span(self, dma_area, buffer_bytes, ring_bytes, copied, delta,
                                    st.hw_ptr_bytes);
            st.hw_ptr_bytes = new_ptr;
            st.copied = Some(absolute);
            st.period_acc += delta;
            if st.period_acc >= PERIOD_BYTES {
                st.period_acc %= PERIOD_BYTES;
                elapsed = true;
                substream = st.substream;
            }
            self.hw_ptr_frames
                .store((new_ptr / FRAME_BYTES) as u32, Relaxed);
        }
        if elapsed {
            // SAFETY: the substream stays valid while it is registered in
            // the stream state (cleared in close under the same lock).
            unsafe { bindings::snd_pcm_period_elapsed(substream) };
        }
        Ok(())
    }
}

unsafe extern "C" fn spkr_dma_callback(param: *mut core::ffi::c_void) {
    // SAFETY: param is the LpmicData the test installed; it outlives the DMA.
    let data = unsafe { &*(param as *const LpmicData) };
    data.spkr_reports.fetch_add(1, Relaxed);
}

/// The amplifier window guard: an hrtimer that drives GPIO105 low when it
/// fires, armed before the pin goes high and cancelled when the thread has
/// lowered the pin itself.
#[pin_data]
struct AmpGuard {
    #[pin]
    timer: HrTimer<Self>,
    #[pin]
    lock: Opaque<bindings::raw_spinlock_t>,
    gpio: usize,
    closed: Atomic<u32>,
    fired: Atomic<u32>,
}

// SAFETY: the opaque raw spinlock is pinned and initialized before use. It
// serializes GPIO writes and the closed state between the task and hard IRQ;
// the remaining shared state is atomic or immutable.
unsafe impl Sync for AmpGuard {}

impl AmpGuard {
    fn new(gpio: *mut bindings::gpio_desc) -> impl PinInit<Self> {
        pin_init!(Self {
            timer <- HrTimer::new(),
            lock <- Opaque::ffi_init(|slot| unsafe { bindings::raw_spin_lock_init(slot) }),
            gpio: gpio as usize,
            closed: Atomic::new(0),
            fired: Atomic::new(0),
        })
    }
}

impl AmpGuard {
    fn enable(&self) -> bool {
        // Expiry and GPIO writes share a lock. A delayed task must not raise
        // the pin after the timer has already muted the amplifier.
        let flags = unsafe { bindings::raw_spin_lock_irqsave(self.lock.get()) };
        let enabled = self.closed.load(Relaxed) == 0;
        if enabled {
            unsafe { bindings::gpiod_set_value(self.gpio as *mut bindings::gpio_desc, 1) };
        }
        unsafe { bindings::raw_spin_unlock_irqrestore(self.lock.get(), flags) };
        enabled
    }

    fn disable(&self) {
        let flags = unsafe { bindings::raw_spin_lock_irqsave(self.lock.get()) };
        unsafe { bindings::gpiod_set_value(self.gpio as *mut bindings::gpio_desc, 0) };
        self.closed.store(1, Relaxed);
        unsafe { bindings::raw_spin_unlock_irqrestore(self.lock.get(), flags) };
    }
}

impl HrTimerCallback for AmpGuard {
    type Pointer<'a> = Pin<&'a Self>;

    fn run(this: Pin<&Self>, _ctx: HrTimerCallbackContext<'_, Self>) -> HrTimerRestart {
        // SAFETY: the descriptor stays valid for the scoped timer's lifetime;
        // the AP GPIO controller is memory-mapped and cannot sleep.
        this.disable();
        this.fired.store(1, Relaxed);
        HrTimerRestart::NoRestart
    }
}

impl_has_hr_timer! {
    impl HasHrTimer<Self> for AmpGuard {
        mode: RelativeMode<Monotonic>, field: self.timer
    }
}

/// The speaker stream start (pwrd + unmute), queued by the back-end DAI's
/// trigger START: those are sleeping firmware calls, and the DMA must already
/// be running when the LEAP starts consuming.
#[pin_data]
struct SpkrStartWork {
    data: Arc<LpmicData>,
    #[pin]
    work: Work<SpkrStartWork>,
}

impl_has_work! {
    impl HasWork<Self> for SpkrStartWork { self.work }
}

impl WorkItem for SpkrStartWork {
    type Pointer = Arc<SpkrStartWork>;

    fn run(this: Arc<SpkrStartWork>) {
        if let Err(e) = this.data.spkr_go() {
            dev_err!(this.data.dev, "spkr: start failed: {:?}\n", e);
        }
    }
}

/// The bench tone, run from the system workqueue after a delay so a
/// microphone capture can be running while the speaker plays.
#[pin_data]
struct SpkrToneWork {
    data: Arc<LpmicData>,
    wave: SpkrWave,
    cancelled: Atomic<u32>,
    #[pin]
    work: DelayedWork<SpkrToneWork>,
}

impl_has_delayed_work! {
    impl HasDelayedWork<Self> for SpkrToneWork { self.work }
}

impl WorkItem for SpkrToneWork {
    type Pointer = Arc<SpkrToneWork>;

    fn run(this: Arc<SpkrToneWork>) {
        if this.cancelled.load(kernel::sync::atomic::Acquire) != 0 {
            return;
        }
        match this.data.speaker_zero_dma_test(this.wave) {
            Ok(()) => {}
            Err(e) => dev_err!(this.data.dev, "speaker bounded tone failed: {:?}\n", e),
        }
    }
}

/// Diagnostic float32 samples. The software stream format is known, but the
/// DMA interpretation and acoustic gain still require independent validation.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum SpkrEnc {
    FloatLe = 2,
}

impl SpkrEnc {
    /// float32 bit pattern of `v / 2^31` built with integer arithmetic (no
    /// floating point in the kernel).
    fn f32_bits_of_scaled(v: i32) -> u32 {
        if v == 0 {
            return 0;
        }
        let sign = if v < 0 { 1u32 << 31 } else { 0 };
        let m = v.unsigned_abs();
        let p = 31 - m.leading_zeros(); // position of the leading one, 0..=31
        // value = m * 2^-31 = 1.f * 2^(p - 31)
        let exp = (p as i32 - 31 + 127) as u32;
        let mant = if p >= 23 {
            (m >> (p - 23)) & 0x7f_ffff
        } else {
            (m << (23 - p)) & 0x7f_ffff
        };
        sign | (exp << 23) | mant
    }

    fn encode(self, v: i32) -> [u8; 4] {
        match self {
            SpkrEnc::FloatLe => Self::f32_bits_of_scaled(v).to_le_bytes(),
        }
    }
}

/// Which waveform a speaker DMA run streams.
#[derive(Clone, Copy)]
enum SpkrWave {
    /// Silence, amplifier never enabled (the zero-DMA proof).
    Silence,
    /// The bounded 1 kHz tone: peak `amp` (re 2^31), `tone_ms` long, amplifier
    /// window `amp_ms` (hrtimer-guarded at SPKR_AMP_GUARD_MS), samples
    /// written with `enc`.
    Tone { amp: u32, tone_ms: u32, amp_ms: u32, enc: SpkrEnc },
}

unsafe extern "C" fn lpmic_pcm_open(substream: *mut bindings::snd_pcm_substream) -> i32 {
    let data = unsafe { Arc::<LpmicData>::borrow((*substream).private_data.cast()) };
    let hw = bindings::snd_pcm_hardware {
        info: bindings::SNDRV_PCM_INFO_MMAP
            | bindings::SNDRV_PCM_INFO_MMAP_VALID
            | bindings::SNDRV_PCM_INFO_INTERLEAVED,
        formats: bindings::BINDINGS_SNDRV_PCM_FMTBIT_S32_LE,
        subformats: 0,
        rates: bindings::SNDRV_PCM_RATE_16000,
        rate_min: RATE,
        rate_max: RATE,
        channels_min: CHANNELS,
        channels_max: CHANNELS,
        buffer_bytes_max: PERIOD_BYTES * PERIODS_MAX as usize,
        period_bytes_min: PERIOD_BYTES,
        period_bytes_max: PERIOD_BYTES,
        periods_min: 2,
        periods_max: PERIODS_MAX,
        fifo_size: 0,
    };
    unsafe {
        (*(*substream).runtime).hw = hw;
    }
    let mut st = data.stream.lock();
    st.substream = substream;
    st.running = false;
    st.copied = None;
    st.hw_ptr_bytes = 0;
    st.period_acc = 0;
    data.hw_ptr_frames.store(0, Relaxed);
    0
}

unsafe extern "C" fn lpmic_pcm_close(substream: *mut bindings::snd_pcm_substream) -> i32 {
    let data = unsafe { Arc::<LpmicData>::borrow((*substream).private_data.cast()) };
    {
        let mut st = data.stream.lock();
        st.running = false;
        st.substream = ptr::null_mut();
    }
    if data.powered.load(Relaxed) != 0 {
        if let Err(e) = data.set_power(POWER_STATE_IDLE) {
            dev_err!(data.dev, "unable to return lpai to idle\n");
            return e.to_errno();
        }
        data.powered.store(0, Relaxed);
    }
    0
}

unsafe extern "C" fn lpmic_pcm_prepare(substream: *mut bindings::snd_pcm_substream) -> i32 {
    let data = unsafe { Arc::<LpmicData>::borrow((*substream).private_data.cast()) };
    let runtime = unsafe { (*substream).runtime };
    {
        let mut st = data.stream.lock();
        st.buffer_bytes = unsafe { (*runtime).dma_bytes };
        st.hw_ptr_bytes = 0;
        st.period_acc = 0;
        st.copied = None;
        data.hw_ptr_frames.store(0, Relaxed);
    }
    if data.powered.load(Relaxed) == 0 {
        if let Err(e) = data.set_power(POWER_STATE_RUN) {
            dev_err!(data.dev, "unable to run lpai\n");
            return e.to_errno();
        }
        data.powered.store(1, Relaxed);
    }
    0
}

unsafe extern "C" fn lpmic_pcm_trigger(substream: *mut bindings::snd_pcm_substream, cmd: i32) -> i32 {
    let data = unsafe { Arc::<LpmicData>::borrow((*substream).private_data.cast()) };
    let mut st = data.stream.lock();
    match cmd as u32 {
        bindings::SNDRV_PCM_TRIGGER_START | bindings::SNDRV_PCM_TRIGGER_RESUME => {
            st.copied = None;
            st.hw_ptr_bytes = 0;
            st.period_acc = 0;
            data.hw_ptr_frames.store(0, Relaxed);
            st.running = true;
            0
        }
        bindings::SNDRV_PCM_TRIGGER_STOP | bindings::SNDRV_PCM_TRIGGER_SUSPEND => {
            st.running = false;
            0
        }
        _ => EINVAL.to_errno(),
    }
}

unsafe extern "C" fn lpmic_pcm_pointer(
    substream: *mut bindings::snd_pcm_substream,
) -> bindings::snd_pcm_uframes_t {
    let data = unsafe { Arc::<LpmicData>::borrow((*substream).private_data.cast()) };
    data.hw_ptr_frames.load(Relaxed) as bindings::snd_pcm_uframes_t
}

unsafe extern "C" fn lpmic_pcm_free_private(pcm: *mut bindings::snd_pcm) {
    unsafe {
        Arc::<LpmicData>::from_foreign((*pcm).private_data.cast());
    }
}

fn copy_str(target: &mut [u8], source: &[u8]) {
    target[..source.len()].copy_from_slice(source)
}

// ---- ASoC component: DPCM front-end PCMs on the ADMAC channels, back-end
// DAIs on the AOP output services (macaudio's t8140 variant links them) ----

/// Front-end PCM DAIs ("j700-pcm-N"): 0 primary playback (the jack),
/// 1 secondary playback (the speaker, later), 2 capture (unused).
const FE_COUNT: usize = 3;
/// DMA channel names per front-end and stream (dma-names on the audio
/// child): [playback, capture].  The primary front-end is the jack: cout on
/// base-ns TX2, cin (the headset microphone) on base-ns RX2.  The speaker
/// front-end's "spkr" channel is requested once the speaker link exists:
/// requesting it touches the LEAP ADMAC.
const FE_DMA_NAMES: [[Option<&core::ffi::CStr>; 2]; FE_COUNT] =
    [[Some(c"cout"), Some(c"cin")], [Some(c"spkr"), None], [None, Some(c"spkr-tap")]];
/// The front-end that plays the internal speaker (j700-pcm-1, the card's
/// "Secondary" PCM): its data goes through `spkr_copy` below.
const FE_SPKR: usize = 1;
/// SNDRV_PCM_FMTBIT_S32_LE (format index 10).
const FMTBIT_S32_LE: u64 = 1 << 10;

/// Digital ceiling of the speaker path, as a Q31 gain applied to every
/// sample userspace writes: 0.05 = -26 dB.
///
/// Measured at the SpeakerTap on 2026-09-17 (hb51, amp off), the words on the
/// speaker wire are exactly `2 × float × 2^31` -- a float of 6 791 000/2^31
/// came back as a clean 1 kHz sine with peak 13 582 000 and nothing else in
/// the path.  So a full-scale S32 sample scaled by 0.05 reaches the MAX98360A
/// as 0.1 × 2^31: the wire never exceeds **-20 dBFS**.  At that level a
/// MAX98360A (about 3 W full scale at 5 V into 4 Ω) delivers ~30 mW, an order
/// of magnitude under the rating of any laptop micro-speaker, indefinitely,
/// without a thermal model.  Apple's own bring-up profile of this speaker's
/// CoreAudio graph (DEVICE_BRING_UP in the AID44 tuning) does the same thing:
/// it replaces the loudspeaker manager with a plain -20 dB gain.
/// Raising this needs a signal-domain protection model fed by the tap.
const SPKR_CEILING_Q31: i64 = 107_374_182;

/// "Speaker Playback Volume": the software gain below the ceiling that the
/// speaker-protection daemon and the card's volume lock drive, 0.5 dB per
/// step from -63.5 dB (1) to 0 dB (127) relative to SPKR_CEILING_Q31, 0 =
/// mute; entry i = round(SPKR_CEILING_Q31 * 10^(-(127 - i) / 40)).
const SPKR_VOLUME_MAX: u32 = 127;
const _: () = assert!(SPKR_VOLUME_Q31[SPKR_VOLUME_MAX as usize] as i64 == SPKR_CEILING_Q31);
const SPKR_VOLUME_Q31: [u32; 128] = [
            0,     76015,     80519,     85290,     90344,     95697,    101368,    107374,
       113737,    120476,    127614,    135176,    143186,    151670,    160657,    170177,
       180260,    190941,    202255,    214240,    226934,    240381,    254624,    269712,
       285693,    302622,    320553,    339547,    359666,    380978,    403552,    427464,
       452793,    479623,    508042,    538146,    570033,    603809,    639587,    677485,
       717629,    760151,    805193,    852903,    903441,    956973,   1013678,   1073742,
      1137365,   1204758,   1276145,   1351761,   1431858,   1516701,   1606571,   1701766,
      1802602,   1909413,   2022553,   2142397,   2269342,   2403809,   2546243,   2697118,
      2856932,   3026216,   3205530,   3395470,   3596664,   3809780,   4035523,   4274643,
      4527932,   4796229,   5080423,   5381457,   5700328,   6038094,   6395874,   6774853,
      7176288,   7601510,   8051928,   8529034,   9034412,   9569734,  10136776,  10737418,
     11373650,  12047581,  12761445,  13517609,  14318577,  15167006,  16065708,  17017661,
     18026021,  19094130,  20225528,  21423966,  22693416,  24038085,  25462431,  26971175,
     28569318,  30262156,  32055302,  33954698,  35966640,  38097797,  40355234,  42746432,
     45279317,  47962285,  50804229,  53814569,  57003283,  60380940,  63958735,  67748529,
     71762881,  76015099,  80519277,  85290344,  90344115,  95697340, 101367764, 107374182,
];
/// The component's only "register": the speaker volume.
const REG_SPKR_VOLUME: u32 = 0;
/// SNDRV_CTL_ELEM_IFACE_MIXER (a __force-cast macro bindgen cannot import).
const SNDRV_CTL_ELEM_IFACE_MIXER: bindings::snd_ctl_elem_iface_t = 2;
/// DECLARE_TLV_DB_SCALE(-6350, 50, 1): SNDRV_CTL_TLVT_DB_SCALE, 8 bytes,
/// min -63.50 dB, 0.50 dB steps, the lowest value mutes.
static SPKR_VOLUME_TLV: [u32; 4] = [1, 8, (-6350i32) as u32, 50 | 0x10000];
/// Managed buffer sizes for the PCMs.
const PCM_BUFFER_PREALLOC: usize = 256 * 1024;
const PCM_BUFFER_MAX: usize = 1024 * 1024;
const RATE_48000_BIT: u32 = 1 << 7; // SNDRV_PCM_RATE_48000

/// Sync wrapper for the static ASoC descriptor tables (raw pointers inside).
struct SyncCell<T>(T);
unsafe impl<T> Sync for SyncCell<T> {}

/// Driver data from an ASoC component (the platform device's drvdata is the
/// pinned `LpmicDriver`).
unsafe fn component_data<'a>(component: *mut bindings::snd_soc_component) -> &'a LpmicData {
    let drv = unsafe { (*(*component).dev).driver_data } as *const LpmicDriver;
    unsafe { &(*drv).data }
}

unsafe fn component_driver<'a>(component: *mut bindings::snd_soc_component) -> &'a LpmicDriver {
    let drv = unsafe { (*(*component).dev).driver_data } as *const LpmicDriver;
    unsafe { &*drv }
}

unsafe fn substream_rtd(substream: *mut bindings::snd_pcm_substream) -> *mut bindings::snd_soc_pcm_runtime {
    unsafe { (*substream).private_data as *mut bindings::snd_soc_pcm_runtime }
}

unsafe fn rtd_is_be(rtd: *mut bindings::snd_soc_pcm_runtime) -> bool {
    unsafe { (*(*rtd).dai_link).no_pcm() != 0 }
}

unsafe fn rtd_cpu_dai(rtd: *mut bindings::snd_soc_pcm_runtime) -> *mut bindings::snd_soc_dai {
    unsafe { *(*rtd).dais }
}

/// DAI ids: 0 means "assign one" to ASoC, so the front-ends carry 1..=3
/// (index + 1) and the back-ends start at 10.
const FE_DAI_ID_BASE: u32 = 1;
const BE_DAI_ID_COUT: u32 = 10;
const BE_DAI_ID_SPKR: u32 = 11;

unsafe fn fe_index(rtd: *mut bindings::snd_soc_pcm_runtime) -> Option<usize> {
    let id = unsafe { (*rtd_cpu_dai(rtd)).id } as u32;
    if id >= FE_DAI_ID_BASE && id < FE_DAI_ID_BASE + FE_COUNT as u32 {
        Some((id - FE_DAI_ID_BASE) as usize)
    } else {
        None
    }
}

unsafe extern "C" fn asoc_pcm_new(
    component: *mut bindings::snd_soc_component,
    rtd: *mut bindings::snd_soc_pcm_runtime,
) -> i32 {
    let data = unsafe { component_data(component) };
    if unsafe { rtd_is_be(rtd) } {
        return 0;
    }
    let Some(fe) = (unsafe { fe_index(rtd) }) else { return EINVAL.to_errno() };
    let pcm = unsafe { (*rtd).pcm };
    for stream in 0..2usize {
        let Some(name) = FE_DMA_NAMES[fe][stream] else { continue };
        let substream = unsafe { (*pcm).streams[stream].substream };
        if substream.is_null() {
            continue;
        }
        // The LEAP ADMAC's SRAM registers only answer with the LEAP leaves
        // powered, so the speaker channel is requested with them on.
        let powered = fe == FE_SPKR && data.spkr_domains(true).is_ok();
        let requested = kernel::error::from_err_ptr(unsafe { bindings::dma_request_chan(data.dev.as_raw(), name.as_ptr()) });
        if powered {
            let _ = data.spkr_domains(false);
        }
        let chan = match requested {
            Ok(c) => c,
            Err(e) => {
                // A front-end can be left without a channel: its PCM exists
                // but opens with ENODEV.
                dev_info!(data.dev, "front-end {}: DMA channel {:?} unavailable: {:?}\n", fe, name, e);
                continue;
            }
        };
        data.fe_chans.lock()[fe][stream] = chan as usize;
        unsafe {
            bindings::snd_pcm_set_managed_buffer(
                substream,
                bindings::SNDRV_DMA_TYPE_DEV as i32,
                (*(*chan).device).dev,
                PCM_BUFFER_PREALLOC,
                PCM_BUFFER_MAX,
            );
        }
    }
    0
}

unsafe extern "C" fn asoc_pcm_free(component: *mut bindings::snd_soc_component, pcm: *mut bindings::snd_pcm) {
    let data = unsafe { component_data(component) };
    let rtd = unsafe { (*pcm).private_data as *mut bindings::snd_soc_pcm_runtime };
    if unsafe { rtd_is_be(rtd) } {
        return;
    }
    let Some(fe) = (unsafe { fe_index(rtd) }) else { return };
    for stream in 0..2 {
        let chan = core::mem::replace(&mut data.fe_chans.lock()[fe][stream], 0);
        if chan != 0 {
            unsafe { bindings::dma_release_channel(chan as *mut bindings::dma_chan) };
        }
    }
}

unsafe extern "C" fn asoc_pcm_open(
    component: *mut bindings::snd_soc_component,
    substream: *mut bindings::snd_pcm_substream,
) -> i32 {
    let data = unsafe { component_data(component) };
    let rtd = unsafe { substream_rtd(substream) };
    if unsafe { rtd_is_be(rtd) } {
        return 0;
    }
    let Some(fe) = (unsafe { fe_index(rtd) }) else { return EINVAL.to_errno() };
    let stream = unsafe { (*substream).stream } as usize;
    let chan = data.fe_chans.lock()[fe][stream & 1] as *mut bindings::dma_chan;
    if chan.is_null() {
        return ENODEV.to_errno();
    }
    let mut hw = bindings::snd_pcm_hardware::default();
    if fe == FE_SPKR {
        // No mmap: every sample goes through spkr_copy's fixed ceiling and
        // the S32 -> float32 conversion the LEAP consumes.
        hw.info = bindings::SNDRV_PCM_INFO_INTERLEAVED;
        hw.formats = FMTBIT_S32_LE;
    } else {
        hw.info = bindings::SNDRV_PCM_INFO_MMAP
            | bindings::SNDRV_PCM_INFO_MMAP_VALID
            | bindings::SNDRV_PCM_INFO_INTERLEAVED;
    }
    hw.periods_min = 2;
    hw.periods_max = u32::MAX;
    hw.period_bytes_min = 256;
    // dma_get_max_seg_size() is inline: the ADMAC sets no dma_parms, SZ_64K.
    hw.period_bytes_max = 0x10000;
    hw.buffer_bytes_max = usize::MAX;
    hw.fifo_size = 16;
    let mut dma_data = bindings::snd_dmaengine_dai_dma_data::default();
    let rc = unsafe {
        bindings::snd_dmaengine_pcm_refine_runtime_hwparams(substream, &mut dma_data, &mut hw, chan)
    };
    if rc < 0 {
        return rc;
    }
    // snd_soc_set_runtime_hwparams() is inline: copy into the runtime.
    unsafe { (*(*substream).runtime).hw = hw };
    unsafe { bindings::snd_dmaengine_pcm_open(substream, chan) }
}

unsafe extern "C" fn asoc_pcm_close(
    _component: *mut bindings::snd_soc_component,
    substream: *mut bindings::snd_pcm_substream,
) -> i32 {
    if unsafe { rtd_is_be(substream_rtd(substream)) } {
        return 0;
    }
    unsafe { bindings::snd_dmaengine_pcm_close(substream) }
}

unsafe extern "C" fn asoc_pcm_hw_params(
    _component: *mut bindings::snd_soc_component,
    substream: *mut bindings::snd_pcm_substream,
    params: *mut bindings::snd_pcm_hw_params,
) -> i32 {
    if unsafe { rtd_is_be(substream_rtd(substream)) } {
        return 0;
    }
    let chan = unsafe { bindings::snd_dmaengine_pcm_get_chan(substream) };
    let mut cfg = bindings::dma_slave_config::default();
    let rc = unsafe { bindings::snd_hwparams_to_dma_slave_config(substream, params, &mut cfg) };
    if rc < 0 {
        return rc;
    }
    // Interval query for the channel count: bindgen has no inline helper.
    let channels = unsafe {
        let iv = &(*params).intervals[(bindings::SNDRV_PCM_HW_PARAM_CHANNELS
            - bindings::SNDRV_PCM_HW_PARAM_FIRST_INTERVAL) as usize];
        iv.min
    };
    let window = core::cmp::min(channels, 4);
    if unsafe { (*substream).stream } == bindings::SNDRV_PCM_STREAM_PLAYBACK as i32 {
        cfg.dst_port_window_size = window;
    } else {
        cfg.src_port_window_size = window;
    }
    unsafe {
        match (*(*chan).device).device_config {
            Some(f) => f(chan, &mut cfg),
            None => ENOSYS.to_errno(),
        }
    }
}

/// The speaker front-end's DMA buffer must never carry stale words: the LEAP
/// starts consuming at pwrd (back-end prepare), before the DMA is started.
unsafe extern "C" fn asoc_pcm_prepare(
    _component: *mut bindings::snd_soc_component,
    substream: *mut bindings::snd_pcm_substream,
) -> i32 {
    let rtd = unsafe { substream_rtd(substream) };
    if unsafe { rtd_is_be(rtd) } || unsafe { fe_index(rtd) } != Some(FE_SPKR) {
        return 0;
    }
    let runtime = unsafe { (*substream).runtime };
    let (area, bytes) = unsafe { ((*runtime).dma_area, (*runtime).dma_bytes) };
    if !area.is_null() && bytes != 0 {
        // SAFETY: the managed buffer is dma_bytes long and no DMA runs before
        // trigger START.
        unsafe { core::ptr::write_bytes(area, 0, bytes) };
    }
    unsafe { component_data(_component) }.spkr_copies.store(0, Relaxed);
    0
}

/// One S32 sample -> the float32 word the LEAP consumes, through the volume
/// table (whose top entry is the ceiling).
fn spkr_f32_bits(sample: i32, gain_q31: u32) -> u32 {
    let scaled = ((sample as i64) * gain_q31 as i64) >> 31; // |scaled| <= SPKR_CEILING_Q31
    SpkrEnc::f32_bits_of_scaled(scaled as i32)
}

/// Copy between userspace and the front-end buffers.  The speaker front-end
/// converts and limits; the jack front-ends copy verbatim (their buffers are
/// also mmap-able, this path serves read()/write()).
unsafe extern "C" fn asoc_pcm_copy(
    _component: *mut bindings::snd_soc_component,
    substream: *mut bindings::snd_pcm_substream,
    _channel: i32,
    pos: usize,
    iter: *mut bindings::iov_iter,
    bytes: usize,
) -> i32 {
    let rtd = unsafe { substream_rtd(substream) };
    if unsafe { rtd_is_be(rtd) } {
        return EINVAL.to_errno();
    }
    let runtime = unsafe { (*substream).runtime };
    let (area, dma_bytes) = unsafe { ((*runtime).dma_area, (*runtime).dma_bytes) };
    if area.is_null() || pos.checked_add(bytes).map_or(true, |end| end > dma_bytes) {
        return EINVAL.to_errno();
    }
    // SAFETY: bounds checked against the managed buffer above.
    let dst = unsafe { core::slice::from_raw_parts_mut(area.add(pos), bytes) };
    let playback = unsafe { substream_is_playback(substream) };
    if !playback {
        // SAFETY: ALSA hands us a live iov_iter for the whole call.
        let iov = unsafe { kernel::iov::IovIterDest::from_raw(iter) };
        return if iov.copy_to_iter(dst) == bytes { 0 } else { EFAULT.to_errno() };
    }
    // SAFETY: as above.
    let iov = unsafe { kernel::iov::IovIterSource::from_raw(iter) };
    if unsafe { fe_index(rtd) } != Some(FE_SPKR) {
        return if iov.copy_from_iter(dst) == bytes { 0 } else { EFAULT.to_errno() };
    }
    // S32_LE interleaved stereo in, float32 LE out, same byte count.
    let data = unsafe { component_data(_component) };
    let n_copy = data.spkr_copies.fetch_add(1, Relaxed);
    let volume = core::cmp::min(data.spkr_volume.load(Relaxed), SPKR_VOLUME_MAX) as usize;
    let gain = SPKR_VOLUME_Q31[volume];
    let mut chunk = [0u8; 512];
    let mut off = 0;
    while off < bytes {
        let n = core::cmp::min(chunk.len(), bytes - off) & !3;
        if n == 0 {
            return EINVAL.to_errno();
        }
        if iov.copy_from_iter(&mut chunk[..n]) != n {
            return EFAULT.to_errno();
        }
        for (i, word) in chunk[..n].chunks_exact(4).enumerate() {
            let sample = i32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            let bits = spkr_f32_bits(sample, gain);
            if n_copy == 0 && off == 0 && i == 0 {
                dev_dbg!(data.dev, "spkr copy: pos {} bytes {} first sample {:#x} -> {:#x}\n", pos, bytes, sample, bits);
            }
            dst[off + i * 4..off + i * 4 + 4].copy_from_slice(&bits.to_le_bytes());
        }
        off += n;
    }
    0
}

unsafe extern "C" fn asoc_pcm_trigger(
    _component: *mut bindings::snd_soc_component,
    substream: *mut bindings::snd_pcm_substream,
    cmd: i32,
) -> i32 {
    if unsafe { rtd_is_be(substream_rtd(substream)) } {
        return 0;
    }
    unsafe { bindings::snd_dmaengine_pcm_trigger(substream, cmd) }
}

unsafe extern "C" fn asoc_pcm_pointer(
    _component: *mut bindings::snd_soc_component,
    substream: *mut bindings::snd_pcm_substream,
) -> bindings::snd_pcm_uframes_t {
    if unsafe { rtd_is_be(substream_rtd(substream)) } {
        return ENOTSUPP_POINTER;
    }
    unsafe { bindings::snd_dmaengine_pcm_pointer(substream) }
}

/// -ENOTSUPP as the pointer callback returns it (mca does the same).
const ENOTSUPP_POINTER: bindings::snd_pcm_uframes_t = (-524i64) as bindings::snd_pcm_uframes_t;

/// The component's register file: the speaker volume, read and written by
/// the standard volsw control helpers.
unsafe extern "C" fn asoc_component_read(component: *mut bindings::snd_soc_component, reg: u32) -> u32 {
    let data = unsafe { component_data(component) };
    match reg {
        REG_SPKR_VOLUME => data.spkr_volume.load(Relaxed),
        _ => 0,
    }
}

unsafe extern "C" fn asoc_component_write(component: *mut bindings::snd_soc_component, reg: u32, val: u32) -> i32 {
    let data = unsafe { component_data(component) };
    match reg {
        REG_SPKR_VOLUME => {
            data.spkr_volume.store(core::cmp::min(val, SPKR_VOLUME_MAX), Relaxed);
            0
        }
        _ => EINVAL.to_errno(),
    }
}

/// Writable, like the C compound literal: the card's volume lock updates
/// `platform_max` in place.
static mut SPKR_VOLUME_MC: bindings::soc_mixer_control = bindings::soc_mixer_control {
    min: 0,
    max: SPKR_VOLUME_MAX as i32,
    platform_max: SPKR_VOLUME_MAX as i32,
    reg: REG_SPKR_VOLUME as i32,
    rreg: REG_SPKR_VOLUME as i32,
    shift: 0,
    rshift: 0,
    // SAFETY: zero is the C initializer's value for the remaining fields
    // (sign_bit, invert, autodisable, topology object).
    ..unsafe { core::mem::zeroed() }
};

/// SOC_SINGLE_TLV("Speaker Playback Volume", REG_SPKR_VOLUME, 0, 127, 0, tlv);
/// `private_value` (the mixer control's address) is filled in at probe, as
/// const evaluation cannot cast a pointer.
static mut ASOC_CONTROLS: [bindings::snd_kcontrol_new; 1] = [bindings::snd_kcontrol_new {
    iface: SNDRV_CTL_ELEM_IFACE_MIXER,
    name: c"Speaker Playback Volume".as_ptr(),
    access: bindings::SNDRV_CTL_ELEM_ACCESS_TLV_READ | bindings::SNDRV_CTL_ELEM_ACCESS_READWRITE,
    info: Some(bindings::snd_soc_info_volsw),
    get: Some(bindings::snd_soc_get_volsw),
    put: Some(bindings::snd_soc_put_volsw),
    tlv: bindings::snd_kcontrol_new__bindgen_ty_1 { p: SPKR_VOLUME_TLV.as_ptr() },
    private_value: 0,
    // SAFETY: zero is a valid value for the remaining fields (device, subdevice,
    // index, count, lock, unlock).
    ..unsafe { core::mem::zeroed() }
}];

static mut ASOC_COMPONENT: bindings::snd_soc_component_driver = bindings::snd_soc_component_driver {
    name: c"j700-aop-audio".as_ptr(),
    read: Some(asoc_component_read),
    write: Some(asoc_component_write),
    controls: ptr::null(),
    num_controls: 1,
    open: Some(asoc_pcm_open),
    close: Some(asoc_pcm_close),
    hw_params: Some(asoc_pcm_hw_params),
    prepare: Some(asoc_pcm_prepare),
    trigger: Some(asoc_pcm_trigger),
    pointer: Some(asoc_pcm_pointer),
    copy: Some(asoc_pcm_copy),
    pcm_new: Some(asoc_pcm_new),
    pcm_free: Some(asoc_pcm_free),
    // SAFETY: all-zero is a valid (empty) component driver; the fields
    // above are the ones this component implements.
    ..unsafe { core::mem::zeroed() }
};

unsafe fn dai_data<'a>(dai: *mut bindings::snd_soc_dai) -> &'a LpmicData {
    unsafe { component_data((*dai).component) }
}

unsafe extern "C" fn dai_accept_fmt(_dai: *mut bindings::snd_soc_dai, _fmt: u32) -> i32 {
    // The AOP runs the serializers: I2S, 32-bit slots, clocks from the firmware.
    0
}

unsafe extern "C" fn dai_accept_sysclk(_dai: *mut bindings::snd_soc_dai, _id: i32, _freq: u32, _dir: i32) -> i32 {
    0
}

unsafe extern "C" fn dai_accept_bclk_ratio(_dai: *mut bindings::snd_soc_dai, _ratio: u32) -> i32 {
    0
}

unsafe fn substream_is_playback(substream: *mut bindings::snd_pcm_substream) -> bool {
    let stream = unsafe { (*substream).stream };
    stream == bindings::SNDRV_PCM_STREAM_PLAYBACK as i32
}

unsafe extern "C" fn cout_dai_startup(substream: *mut bindings::snd_pcm_substream, dai: *mut bindings::snd_soc_dai) -> i32 {
    let data = unsafe { dai_data(dai) };
    match data.jack_startup(unsafe { substream_is_playback(substream) }) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

unsafe extern "C" fn cout_dai_prepare(substream: *mut bindings::snd_pcm_substream, dai: *mut bindings::snd_soc_dai) -> i32 {
    let data = unsafe { dai_data(dai) };
    match data.jack_prepare(unsafe { substream_is_playback(substream) }) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

unsafe extern "C" fn cout_dai_shutdown(substream: *mut bindings::snd_pcm_substream, dai: *mut bindings::snd_soc_dai) {
    let data = unsafe { dai_data(dai) };
    data.jack_shutdown(unsafe { substream_is_playback(substream) });
}

unsafe extern "C" fn spkr_dai_startup(substream: *mut bindings::snd_pcm_substream, dai: *mut bindings::snd_soc_dai) -> i32 {
    let data = unsafe { dai_data(dai) };
    let ret = if unsafe { substream_is_playback(substream) } {
        data.spkr_startup()
    } else {
        data.sense_startup()
    };
    match ret {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

/// The sense (capture) side runs from prepare: the RX DMA is armed by then
/// and a capture may start before or after the speaker plays.
unsafe extern "C" fn spkr_dai_prepare(substream: *mut bindings::snd_pcm_substream, dai: *mut bindings::snd_soc_dai) -> i32 {
    if unsafe { substream_is_playback(substream) } {
        return 0;
    }
    let data = unsafe { dai_data(dai) };
    match data.sense_prepare() {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

/// Atomic context: queue the sleeping start, or note the stop (playback
/// only; the sense side needs nothing here).
unsafe extern "C" fn spkr_dai_trigger(substream: *mut bindings::snd_pcm_substream, cmd: i32, dai: *mut bindings::snd_soc_dai) -> i32 {
    if !unsafe { substream_is_playback(substream) } {
        return 0;
    }
    let drv = unsafe { component_driver((*dai).component) };
    match cmd as u32 {
        bindings::SNDRV_PCM_TRIGGER_START
        | bindings::SNDRV_PCM_TRIGGER_RESUME
        | bindings::SNDRV_PCM_TRIGGER_PAUSE_RELEASE => {
            drv.data.spkr_want_run.store(1, Relaxed);
            if let Some(work) = drv.spkr_start.as_ref() {
                // Err = already queued: the pending run will see want_run.
                let _ = workqueue::system().enqueue(work.clone());
            }
            0
        }
        bindings::SNDRV_PCM_TRIGGER_STOP
        | bindings::SNDRV_PCM_TRIGGER_SUSPEND
        | bindings::SNDRV_PCM_TRIGGER_PAUSE_PUSH => {
            drv.data.spkr_want_run.store(0, Relaxed);
            0
        }
        _ => EINVAL.to_errno(),
    }
}

unsafe extern "C" fn spkr_dai_shutdown(substream: *mut bindings::snd_pcm_substream, dai: *mut bindings::snd_soc_dai) {
    let drv = unsafe { component_driver((*dai).component) };
    if !unsafe { substream_is_playback(substream) } {
        drv.data.sense_shutdown();
        return;
    }
    drv.data.spkr_want_run.store(0, Relaxed);
    if let Some(work) = drv.spkr_start.as_ref() {
        // A start still queued or running must finish before the mute/pw0.
        unsafe {
            let raw = workqueue::Work::raw_get(&work.work);
            bindings::flush_work(raw);
        }
    }
    drv.data.spkr_shutdown();
}

static SPKR_DAI_OPS: SyncCell<bindings::snd_soc_dai_ops> = SyncCell(bindings::snd_soc_dai_ops {
    startup: Some(spkr_dai_startup),
    prepare: Some(spkr_dai_prepare),
    trigger: Some(spkr_dai_trigger),
    shutdown: Some(spkr_dai_shutdown),
    set_fmt: Some(dai_accept_fmt),
    set_sysclk: Some(dai_accept_sysclk),
    set_bclk_ratio: Some(dai_accept_bclk_ratio),
    // SAFETY: zero is an empty ops table.
    ..unsafe { core::mem::zeroed() }
});

static COUT_DAI_OPS: SyncCell<bindings::snd_soc_dai_ops> = SyncCell(bindings::snd_soc_dai_ops {
    startup: Some(cout_dai_startup),
    prepare: Some(cout_dai_prepare),
    shutdown: Some(cout_dai_shutdown),
    set_fmt: Some(dai_accept_fmt),
    set_sysclk: Some(dai_accept_sysclk),
    set_bclk_ratio: Some(dai_accept_bclk_ratio),
    // SAFETY: zero is an empty ops table.
    ..unsafe { core::mem::zeroed() }
});

static FE_DAI_OPS: SyncCell<bindings::snd_soc_dai_ops> = SyncCell(bindings::snd_soc_dai_ops {
    set_fmt: Some(dai_accept_fmt),
    set_sysclk: Some(dai_accept_sysclk),
    set_bclk_ratio: Some(dai_accept_bclk_ratio),
    // SAFETY: zero is an empty ops table.
    ..unsafe { core::mem::zeroed() }
});

const fn pcm_stream_ch(name: &'static core::ffi::CStr, formats: u64, channels: u32) -> bindings::snd_soc_pcm_stream {
    bindings::snd_soc_pcm_stream {
        stream_name: name.as_ptr(),
        formats,
        subformats: 0,
        rates: RATE_48000_BIT,
        rate_min: 48000,
        rate_max: 48000,
        channels_min: channels,
        channels_max: channels,
        sig_bits: 0,
    }
}

const fn pcm_stream(name: &'static core::ffi::CStr, formats: u64) -> bindings::snd_soc_pcm_stream {
    pcm_stream_ch(name, formats, 2)
}

const fn empty_stream() -> bindings::snd_soc_pcm_stream {
    bindings::snd_soc_pcm_stream {
        stream_name: ptr::null(),
        formats: 0,
        subformats: 0,
        rates: 0,
        rate_min: 0,
        rate_max: 0,
        channels_min: 0,
        channels_max: 0,
        sig_bits: 0,
    }
}

/// The AOP-configured serializers take 24-bit samples from the low bits
/// of each 32-bit DMA word (macOS "slot: 32bits smpl: 24bits"): S24_LE.
const OUT_FORMATS: u64 = bindings::BINDINGS_SNDRV_PCM_FMTBIT_S24_LE;

/// DAI table: [0] the cout back-end (DT index 0), the front-end PCMs, then
/// [4] the speaker back-end (DT index 4).
static ASOC_DAIS: SyncCell<[bindings::snd_soc_dai_driver; 2 + FE_COUNT]> = SyncCell([
    bindings::snd_soc_dai_driver {
        name: c"cout".as_ptr(),
        id: BE_DAI_ID_COUT,
        ops: &COUT_DAI_OPS.0,
        playback: pcm_stream(c"COUT TX", OUT_FORMATS),
        // cin: the headset microphone, one 32-bit slot per frame
        capture: pcm_stream_ch(c"CIN RX", OUT_FORMATS, 1),
        // SAFETY: zero is a valid base for the remaining fields.
        ..unsafe { core::mem::zeroed() }
    },
    bindings::snd_soc_dai_driver {
        name: c"j700-pcm-0".as_ptr(),
        id: FE_DAI_ID_BASE,
        ops: &FE_DAI_OPS.0,
        playback: pcm_stream(c"PCM0 TX", OUT_FORMATS),
        // macaudio routes "Headset Capture" into PCM0 RX (mono cin)
        capture: pcm_stream_ch(c"PCM0 RX", OUT_FORMATS, 1),
        ..unsafe { core::mem::zeroed() }
    },
    bindings::snd_soc_dai_driver {
        name: c"j700-pcm-1".as_ptr(),
        id: FE_DAI_ID_BASE + 1,
        ops: &FE_DAI_OPS.0,
        // the speaker front-end: S32 in, converted through the ceiling
        playback: pcm_stream(c"PCM1 TX", FMTBIT_S32_LE),
        capture: empty_stream(),
        ..unsafe { core::mem::zeroed() }
    },
    bindings::snd_soc_dai_driver {
        name: c"j700-pcm-2".as_ptr(),
        id: FE_DAI_ID_BASE + 2,
        ops: &FE_DAI_OPS.0,
        playback: empty_stream(),
        // the speaker sense front-end: the words on the speaker wire
        capture: pcm_stream(c"PCM2 RX", FMTBIT_S32_LE),
        ..unsafe { core::mem::zeroed() }
    },
    // The internal speaker back-end: LEAP TX0 under the AOP spkr service,
    // the MAX98360A's SD_MODE driven by its own codec driver; its capture
    // side is the SpeakerTap (base-ns RX1, the tap service).
    bindings::snd_soc_dai_driver {
        name: c"spkr".as_ptr(),
        id: BE_DAI_ID_SPKR,
        ops: &SPKR_DAI_OPS.0,
        playback: pcm_stream(c"SPKR TX", FMTBIT_S32_LE),
        capture: pcm_stream(c"SPKR RX", FMTBIT_S32_LE),
        ..unsafe { core::mem::zeroed() }
    },
]);

struct LpmicDriver {
    card: *mut bindings::snd_card,
    data: Arc<LpmicData>,
    asoc_registered: bool,
    speaker_work: Option<Arc<SpkrToneWork>>,
    spkr_start: Option<Arc<SpkrStartWork>>,
}

impl LpmicDriver {
    const VTABLE: bindings::snd_pcm_ops = bindings::snd_pcm_ops {
        open: Some(lpmic_pcm_open),
        close: Some(lpmic_pcm_close),
        prepare: Some(lpmic_pcm_prepare),
        trigger: Some(lpmic_pcm_trigger),
        pointer: Some(lpmic_pcm_pointer),
        ioctl: None,
        hw_params: None,
        hw_free: None,
        sync_stop: None,
        get_time_info: None,
        fill_silence: None,
        copy: None,
        page: None,
        mmap: None,
        ack: None,
    };

    fn new(data: Arc<LpmicData>) -> Result<Self> {
        let start_data = data.clone();
        let spkr_start = Arc::pin_init(
            pin_init!(SpkrStartWork {
                data: start_data,
                work <- new_work!("SpkrStartWork::work"),
            }),
            GFP_KERNEL,
        )?;
        let mut this = LpmicDriver {
            card: ptr::null_mut(),
            data: data.clone(),
            asoc_registered: false,
            speaker_work: None,
            spkr_start: Some(spkr_start),
        };
        let ret = unsafe {
            bindings::snd_card_new(
                data.dev.as_raw(),
                -1,
                c_str!("NeoMic").as_char_ptr(),
                THIS_MODULE.as_ptr(),
                0,
                &mut this.card,
            )
        };
        if ret < 0 {
            dev_err!(data.dev, "Unable to allocate sound card\n");
            return Err(Error::from_errno(ret));
        }
        unsafe {
            copy_str(&mut (*this.card).driver, c"j700_lpmic".to_bytes_with_nul());
            copy_str(&mut (*this.card).id, c"NeoMic".to_bytes_with_nul());
            copy_str(&mut (*this.card).shortname, c"MacBook Neo LPMic".to_bytes_with_nul());
            copy_str(
                &mut (*this.card).longname,
                c"Apple MacBook Neo Low-Power Microphone".to_bytes_with_nul(),
            );
        }

        let mut pcm = ptr::null_mut();
        let ret = unsafe {
            bindings::snd_pcm_new(this.card, c"lpmic".as_ptr() as _, 0, 0, 1, &mut pcm)
        };
        if ret < 0 {
            dev_err!(data.dev, "Unable to allocate PCM device\n");
            return Err(Error::from_errno(ret));
        }
        unsafe {
            bindings::snd_pcm_set_ops(pcm, bindings::SNDRV_PCM_STREAM_CAPTURE as i32, &Self::VTABLE);
            (*pcm).private_data = data.clone().into_foreign() as _;
            (*pcm).private_free = Some(lpmic_pcm_free_private);
            (*pcm).info_flags = 0;
            copy_str(&mut (*pcm).name, c"lpmic".to_bytes_with_nul());
            bindings::snd_pcm_set_managed_buffer_all(
                pcm,
                bindings::SNDRV_DMA_TYPE_VMALLOC as i32,
                ptr::null_mut(),
                PERIOD_BYTES * 4,
                PERIOD_BYTES * PERIODS_MAX as usize,
            );
        }

        let ret = unsafe { bindings::snd_card_register(this.card) };
        if ret < 0 {
            dev_err!(data.dev, "Unable to register sound card\n");
            return Err(Error::from_errno(ret));
        }

        // The ASoC side: the jack (cout) and speaker back-ends, the
        // front-end PCMs and the speaker volume control.
        // SAFETY: probe runs once at a time; the statics are only completed
        // here, before the component is registered, and never changed again.
        let ret = unsafe {
            let controls = &raw mut ASOC_CONTROLS;
            (*controls)[0].private_value = (&raw mut SPKR_VOLUME_MC) as usize as _;
            let component = &raw mut ASOC_COMPONENT;
            (*component).controls = (*controls).as_ptr();
            bindings::snd_soc_register_component(
                data.dev.as_raw(),
                component,
                ASOC_DAIS.0.as_ptr() as *mut bindings::snd_soc_dai_driver,
                ASOC_DAIS.0.len() as i32,
            )
        };
        if ret < 0 {
            dev_err!(data.dev, "unable to register the ASoC component: {}\n", ret);
            return Err(Error::from_errno(ret));
        }
        this.asoc_registered = true;
        Ok(this)
    }
}

impl Drop for LpmicDriver {
    fn drop(&mut self) {
        if let Some(work) = self.speaker_work.take() {
            work.cancelled.store(1, kernel::sync::atomic::Release);
            // Flush a queued item as a no-op, or wait for an in-flight bounded
            // test, before unregistering devices or unloading module text.
            unsafe {
                let raw = workqueue::Work::raw_get(DelayedWork::raw_as_work(&work.work));
                bindings::flush_delayed_work(raw.cast());
            }
        }
        if self.asoc_registered {
            unsafe { bindings::snd_soc_unregister_component_by_driver(self.data.dev.as_raw(), ptr::null()) };
        }
        if let Some(work) = self.spkr_start.take() {
            self.data.spkr_want_run.store(0, Relaxed);
            unsafe {
                let raw = workqueue::Work::raw_get(&work.work);
                bindings::flush_work(raw);
            }
        }
        for slot in [&self.data.pd_mca1, &self.data.pd_mca0, &self.data.pd_tx0, &self.data.pd_leap_mca] {
            let pd = slot.xchg(0, Relaxed);
            if pd != 0 {
                unsafe { bindings::dev_pm_domain_detach(pd as *mut bindings::device, true) };
            }
        }
        if !self.card.is_null() {
            unsafe {
                bindings::snd_card_free(self.card);
            }
        }
        // The AOP keeps dispatching producer reports to the first listener
        // registered for the service: unhook this instance so a reload sees
        // its own reports.
        self.data
            .adata
            .remove_report_listener(&self.data.service, EPIC_SUBTYPE_PRODUCER_REPORT);
        if self.data.powered.load(Relaxed) != 0 {
            if self.data.set_power(POWER_STATE_IDLE).is_ok() {
                self.data.powered.store(0, Relaxed);
            }
        }
    }
}

unsafe impl Send for LpmicDriver {}
unsafe impl Sync for LpmicDriver {}

kernel::of_device_table!(
    OF_TABLE,
    MODULE_OF_TABLE,
    <LpmicDriver as platform::Driver>::IdInfo,
    [(of::DeviceId::new(c_str!("apple,t8140-aop-audio")), ())]
);

impl platform::Driver for LpmicDriver {
    type IdInfo = ();

    const OF_ID_TABLE: Option<of::IdTable<Self::IdInfo>> = Some(&OF_TABLE);

    fn probe(pdev: &platform::Device<Core>, _info: Option<&Self::IdInfo>) -> impl PinInit<Self, Error> {
        let dev = ARef::<device::Device>::from(pdev.as_ref());
        let parent = pdev.as_ref().parent().ok_or(ENODEV)?;
        // SAFETY: our parent is the AOP, whose drvdata is a pinned Arc<dyn AOP>.
        let adata_ptr = unsafe { Pin::<KBox<Arc<dyn AOP>>>::borrow(parent.get_drvdata()) };
        let adata = (&*adata_ptr).clone();
        // SAFETY: the AOP driver sets the platform data to the EPIC service.
        let svc = unsafe { *((*dev.as_raw()).platform_data as *const EPICService) };

        // The source ring: 512 KiB through our own IOMMU stream (dart-aop 9),
        // owned by the AOP driver because the firmware binds it once per boot.
        // SAFETY: no DMA is in flight on this device yet.
        unsafe { pdev.dma_set_mask_and_coherent(DmaMask::new::<42>())? };
        // SAFETY: the device is bound while probe runs.
        let bound = unsafe { dev.as_bound() };
        let ring = adata.source_ring(bound, RING_BYTES)?;
        let ring_iova = ring.iova;

        let data_dev = dev.clone();
        let data = Arc::pin_init(
            pin_init!(LpmicData {
                dev: data_dev,
                adata,
                service: svc,
                ring,
                ring_bytes: Atomic::new(0),
                sequence: Atomic::new(0),
                powered: Atomic::new(0),
                hw_ptr_frames: Atomic::new(0),
                spkr_sequence: Atomic::new(0),
                spkr_reports: Atomic::new(0),
                tap_sequence: Atomic::new(0),
                tap_running: Atomic::new(0),
                pd_mca0: Atomic::new(0),
                cout_sequence: Atomic::new(0),
                cout_running: Atomic::new(0),
                cin_sequence: Atomic::new(0),
                cin_running: Atomic::new(0),
                pd_mca1: Atomic::new(0),
                pd_tx0: Atomic::new(0),
                pd_leap_mca: Atomic::new(0),
                spkr_running: Atomic::new(0),
                spkr_want_run: Atomic::new(0),
                spkr_copies: Atomic::new(0),
                spkr_volume: Atomic::new(SPKR_VOLUME_MAX),
                fe_chans <- new_spinlock!([[0usize; 2]; FE_COUNT]),
                stream <- new_spinlock!(StreamState {
                    substream: ptr::null_mut(),
                    running: false,
                    last_absolute: None,
                    copied: None,
                    hw_ptr_bytes: 0,
                    period_acc: 0,
                    buffer_bytes: 0,
                    overruns: 0,
                }),
            }),
            GFP_KERNEL,
        )?;

        data.attach_lpai()?;
        data.set_channel_control()?;
        data.read_geometry()?;
        let listener: Arc<dyn ReportListener> = data.clone();
        data.adata
            .add_report_listener(svc, EPIC_SUBTYPE_PRODUCER_REPORT, listener)?;
        dev_info!(dev, "J700 low-power microphone ready (source ring at {:#x})\n", ring_iova);
        // The jack path's serializer domain (audio_mca1_m), held for the
        // driver's lifetime and runtime-powered around cout streams.
        let pd = unsafe { bindings::dev_pm_domain_attach_by_name(dev.as_raw(), c"mca1".as_ptr()) };
        match kernel::error::from_err_ptr(pd) {
            Ok(p) if !p.is_null() => data.pd_mca1.store(p as usize, Relaxed),
            Ok(_) => dev_warn!(dev, "no mca1 power domain: the jack path will not power its serializer\n"),
            Err(e) => dev_warn!(dev, "mca1 power domain attach failed: {:?}\n", e),
        }
        // The speaker path's LEAP leaves (tx0 -> leap_c -> leap_a -> audio_fr
        // -> audio_a, leap_mca -> audio_p), runtime-powered around streams.
        for (name, slot) in [(c"tx0", &data.pd_tx0), (c"mca", &data.pd_leap_mca), (c"mca0", &data.pd_mca0)] {
            let pd = unsafe { bindings::dev_pm_domain_attach_by_name(dev.as_raw(), name.as_ptr()) };
            match kernel::error::from_err_ptr(pd) {
                Ok(p) if !p.is_null() => slot.store(p as usize, Relaxed),
                Ok(_) => dev_warn!(dev, "no {:?} power domain: the speaker or sense path is unavailable\n", name),
                Err(e) => dev_warn!(dev, "{:?} power domain attach failed: {:?}\n", name, e),
            }
        }
        if *module_parameters::spkr_test.value() != 0 {
            match data.speaker_zero_dma_test(SpkrWave::Silence) {
                Ok(()) => {}
                Err(e) => dev_err!(dev, "speaker zero-DMA test failed: {:?}\n", e),
            }
        }
        if *module_parameters::spkr_props.value() != 0 {
            // Silent: attach and read the descriptors the firmware exposes.
            if data.attach_device(AUDIO_DEV_SPKR, "spkr").is_ok() {
                for m in [300u32, 301, 302, 303, 304, 305, 400, 401, 402, 403, 404, 405, 500, 501, 700] {
                    data.dump_prop(AUDIO_DEV_SPKR, "spkr", m);
                }
            }
            for m in [301u32, 303, 304, 305] {
                data.dump_prop(AUDIO_DEV_LPAI, "lpai", m);
            }
        }
        let mut driver = Self::new(data.clone())?;
        let tone_amp = *module_parameters::spkr_tone_amp.value();
        if tone_amp != 0 {
            let tone_ms = core::cmp::min(*module_parameters::spkr_tone_ms.value(), SPKR_TONE_MS_MAX);
            let amp_ms = core::cmp::min(*module_parameters::spkr_amp_ms.value(), SPKR_AMP_MS_MAX);
            let delay_ms = *module_parameters::spkr_tone_delay_ms.value();
            if tone_amp > SPKR_TONE_AMP_MAX {
                dev_err!(dev, "spkr_tone_amp {} exceeds the ceiling {}, refusing\n",
                         tone_amp, SPKR_TONE_AMP_MAX);
            } else if *module_parameters::spkr_tone_enc.value() != 2 {
                dev_err!(dev, "speaker bench requires explicit float LE encoding; integer experiments are disabled\n");
                return Err(EINVAL);
            } else {
                let enc = SpkrEnc::FloatLe;
                let wave = SpkrWave::Tone { amp: tone_amp, tone_ms, amp_ms, enc };
                let work_data = data.clone();
                let work = Arc::pin_init(
                    pin_init!(SpkrToneWork {
                        data: work_data,
                        wave,
                        cancelled: Atomic::new(0),
                        work <- new_delayed_work!("SpkrToneWork::work"),
                    }),
                    GFP_KERNEL,
                )?;
                dev_info!(dev, "spkr: bounded tone (amp {}) scheduled in {} ms\n", tone_amp, delay_ms);
                driver.speaker_work = Some(work.clone());
                let _ = workqueue::system().enqueue_delayed(work, msecs_to_jiffies(delay_ms));
            }
        }
        Ok(driver)
    }
}

module_platform_driver! {
    type: LpmicDriver,
    name: "snd_soc_t8140_aop_audio",
    description: "Apple T8140 (J700) AOP audio: low-power microphone, jack and speaker paths",
    license: "Dual MIT/GPL",
    params: {
        spkr_test: u8 {
            default: 0,
            description: "Bench: run the internal speaker zero-DMA proof at probe (no amplifier enable)",
        },
        spkr_tone_amp: u32 {
            default: 0,
            description: "Bench: S32 peak of a bounded 1 kHz tone at probe (0 = off, ceiling 6791000 = -50 dBFS)",
        },
        spkr_tone_ms: u32 {
            default: 100,
            description: "Bench: bounded tone length in ms (max 150)",
        },
        spkr_amp_ms: u32 {
            default: 250,
            description: "Bench: amplifier enable window in ms (max 280; hrtimer guard at 300)",
        },
        spkr_tone_delay_ms: u32 {
            default: 0,
            description: "Bench: delay before the bounded tone, so a capture can run meanwhile",
        },
        spkr_props: u8 {
            default: 0,
            description: "Bench: dump the spkr/lpai device property descriptors at probe (silent)",
        },
        spkr_tone_enc: u32 {
            default: 0,
            description: "Bench: must explicitly select 2 for float LE; all other encodings are rejected",
        },
    },
}

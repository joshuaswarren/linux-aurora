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
    types::ForeignOwnable,
    workqueue::{
        self,
        DelayedWork,
        WorkItem, //
    }, //
};
use kernel::impl_has_hr_timer;
use kernel::time::msecs_to_jiffies;
use kernel::workqueue::{impl_has_delayed_work, new_delayed_work};
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
/// Hard digital ceiling for the bench tone: -50 dBFS re 2^31.
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

fn cntvct() -> u64 {
    let v: u64;
    // SAFETY: reading the architectural counter has no side effects.
    unsafe {
        asm!("mrs {}, cntvct_el0", out(reg) v);
    }
    v
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

/// Property 202 on the spkr service: FourCC, sequence, CNTVCT timestamp, a
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
    cout_sequence: Atomic<u32>,
    cout_running: Atomic<u32>,
    cin_sequence: Atomic<u32>,
    cin_running: Atomic<u32>,
    /// audio_mca1_m virtual power-domain device (dev_pm_domain_attach_by_name), 0 if none.
    pd_mca1: Atomic<usize>,
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
            timestamp: cntvct(),
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

    /// Output device (spkr/cout) power request: property 202 with a per-
    /// device sequence and the CNTVCT timestamp; pw0 is confirmed through
    /// property 200 like the native host did.
    fn out_set_power(&self, dev_id: u32, name: &str, seq: &Atomic<u32>, state: u32) -> Result<()> {
        let sequence = seq.fetch_add(1, Relaxed) + 1;
        let req = SpkrPowerRequest {
            dev_id,
            sequence,
            timestamp: cntvct(),
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
                unsafe { bindings::dev_pm_domain_detach(pd, true) };
                return Err(EIO);
            }
            pds[i] = pd;
        }
        dev_info!(self.dev, "spkr: LEAP power domains up\n");

        let result = self.speaker_zero_dma_run(wave);

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
        let chan = unsafe {
            kernel::error::from_err_ptr(bindings::dma_request_chan(
                self.dev.as_raw(),
                c_str!("spkr").as_char_ptr(),
            ))
        }?;
        let res = (|| -> Result<()> {
            // The buffer must be mapped through the DMA controller's own
            // stream (dart-aop 10), so allocate it on the controller device.
            // SAFETY: the channel holds the controller bound for its lifetime.
            let dma_dev: &device::Device = unsafe { device::Device::from_raw((*(*chan).device).dev) };
            let bound = unsafe { dma_dev.as_bound() };
            let buf = Coherent::<u8>::zeroed_slice(bound, buf_bytes, GFP_KERNEL)?;
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
            self.spkr_set_power(SPKR_POWER_STATE_PWRD)?;
            if let (SpkrWave::Tone { amp, tone_ms, amp_ms, enc }, Some(gpio)) = (wave, amp_gpio) {
                // The amplifier window: pin high, sleep, pin low, with the
                // hrtimer forcing it low at SPKR_AMP_GUARD_MS regardless.
                let t0 = Instant::<Monotonic>::now();
                stack_pin_init!(let guard = AmpGuard::new(gpio));
                guard.as_ref().start_scoped(Delta::from_millis(SPKR_AMP_GUARD_MS), || {
                    unsafe { bindings::gpiod_set_value(gpio, 1) };
                    kernel::time::delay::fsleep(Delta::from_millis(amp_ms as i64));
                    unsafe { bindings::gpiod_set_value(gpio, 0) };
                });
                let window_ms = t0.elapsed().as_millis();
                let low = unsafe { bindings::gpiod_get_value(gpio) } == 0;
                dev_info!(self.dev,
                          "J700 SPEAKER_TONE_WINDOW: amp {} enc {} ({} ms tone), enable {} ms, guard fired {}, pin low {}\n",
                          amp, enc as u32, tone_ms, window_ms, guard.fired.load(Relaxed), low);
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
            self.spkr_set_power(SPKR_POWER_STATE_PW0)?;
            unsafe {
                if let Some(f) = (*(*chan).device).device_terminate_all {
                    f(chan);
                }
            }
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
        unsafe { bindings::dma_release_channel(chan) };
        if let Some(gpio) = amp_gpio {
            unsafe {
                bindings::gpiod_set_value(gpio, 0);
                bindings::gpiod_put(gpio);
            }
        }
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
    gpio: usize,
    fired: Atomic<u32>,
}

impl AmpGuard {
    fn new(gpio: *mut bindings::gpio_desc) -> impl PinInit<Self> {
        pin_init!(Self {
            timer <- HrTimer::new(),
            gpio: gpio as usize,
            fired: Atomic::new(0),
        })
    }
}

impl HrTimerCallback for AmpGuard {
    type Pointer<'a> = Pin<&'a Self>;

    fn run(this: Pin<&Self>, _ctx: HrTimerCallbackContext<'_, Self>) -> HrTimerRestart {
        // SAFETY: the descriptor stays valid for the scoped timer's lifetime;
        // the AP GPIO controller is memory-mapped and cannot sleep.
        unsafe { bindings::gpiod_set_value(this.gpio as *mut bindings::gpio_desc, 0) };
        this.fired.store(1, Relaxed);
        HrTimerRestart::NoRestart
    }
}

impl_has_hr_timer! {
    impl HasHrTimer<Self> for AmpGuard {
        mode: RelativeMode<Monotonic>, field: self.timer
    }
}

/// The bench tone, run from the system workqueue after a delay so a
/// microphone capture can be running while the speaker plays.
#[pin_data]
struct SpkrToneWork {
    data: Arc<LpmicData>,
    wave: SpkrWave,
    #[pin]
    work: DelayedWork<SpkrToneWork>,
}

impl_has_delayed_work! {
    impl HasDelayedWork<Self> for SpkrToneWork { self.work }
}

impl WorkItem for SpkrToneWork {
    type Pointer = Arc<SpkrToneWork>;

    fn run(this: Arc<SpkrToneWork>) {
        match this.data.speaker_zero_dma_test(this.wave) {
            Ok(()) => {}
            Err(e) => dev_err!(this.data.dev, "speaker bounded tone failed: {:?}\n", e),
        }
    }
}

/// How a tone sample is written into its 32-bit slot (bench: the LEAP's
/// sample representation is being established).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SpkrEnc {
    /// Little-endian signed 32-bit integer.
    IntLe,
    /// Big-endian signed 32-bit integer.
    IntBe,
    /// Little-endian IEEE-754 float32 of amp / 2^31.
    FloatLe,
    /// Big-endian IEEE-754 float32 of amp / 2^31.
    FloatBe,
}

impl SpkrEnc {
    fn from_param(v: u32) -> Self {
        match v {
            1 => SpkrEnc::IntBe,
            2 => SpkrEnc::FloatLe,
            3 => SpkrEnc::FloatBe,
            _ => SpkrEnc::IntLe,
        }
    }

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
            SpkrEnc::IntLe => v.to_le_bytes(),
            SpkrEnc::IntBe => v.to_be_bytes(),
            SpkrEnc::FloatLe => Self::f32_bits_of_scaled(v).to_le_bytes(),
            SpkrEnc::FloatBe => Self::f32_bits_of_scaled(v).to_be_bytes(),
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
    [[Some(c"cout"), Some(c"cin")], [None, None], [None, None]];
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
        let chan = match kernel::error::from_err_ptr(unsafe { bindings::dma_request_chan(data.dev.as_raw(), name.as_ptr()) }) {
            Ok(c) => c,
            Err(e) => {
                // The LEAP ADMAC's SRAM registers only answer with the LEAP
                // leaves powered (they are off until the speaker back-end
                // runs), so a front-end can be left without a channel: its
                // PCM exists but opens with ENODEV.
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
    hw.info = bindings::SNDRV_PCM_INFO_MMAP
        | bindings::SNDRV_PCM_INFO_MMAP_VALID
        | bindings::SNDRV_PCM_INFO_INTERLEAVED;
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

static ASOC_COMPONENT: SyncCell<bindings::snd_soc_component_driver> = SyncCell(bindings::snd_soc_component_driver {
    name: c"j700-aop-audio".as_ptr(),
    open: Some(asoc_pcm_open),
    close: Some(asoc_pcm_close),
    hw_params: Some(asoc_pcm_hw_params),
    trigger: Some(asoc_pcm_trigger),
    pointer: Some(asoc_pcm_pointer),
    pcm_new: Some(asoc_pcm_new),
    pcm_free: Some(asoc_pcm_free),
    // SAFETY: all-zero is a valid (empty) component driver; the fields
    // above are the ones this component implements.
    ..unsafe { core::mem::zeroed() }
});

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

/// DAI table: [0] the cout back-end (DT index 0), then the front-end PCMs.
static ASOC_DAIS: SyncCell<[bindings::snd_soc_dai_driver; 1 + FE_COUNT]> = SyncCell([
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
        playback: pcm_stream(c"PCM1 TX", OUT_FORMATS),
        capture: empty_stream(),
        ..unsafe { core::mem::zeroed() }
    },
    bindings::snd_soc_dai_driver {
        name: c"j700-pcm-2".as_ptr(),
        id: FE_DAI_ID_BASE + 2,
        ops: &FE_DAI_OPS.0,
        playback: empty_stream(),
        capture: pcm_stream(c"PCM2 RX", OUT_FORMATS),
        ..unsafe { core::mem::zeroed() }
    },
]);

struct LpmicDriver {
    card: *mut bindings::snd_card,
    data: Arc<LpmicData>,
    asoc_registered: bool,
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
        let mut this = LpmicDriver {
            card: ptr::null_mut(),
            data: data.clone(),
            asoc_registered: false,
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

        // The ASoC side: the jack (cout) back-end and the front-end PCMs.
        let ret = unsafe {
            bindings::snd_soc_register_component(
                data.dev.as_raw(),
                &ASOC_COMPONENT.0,
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
        if self.asoc_registered {
            unsafe { bindings::snd_soc_unregister_component_by_driver(self.data.dev.as_raw(), ptr::null()) };
        }
        let pd = self.data.pd_mca1.xchg(0, Relaxed);
        if pd != 0 {
            unsafe { bindings::dev_pm_domain_detach(pd as *mut bindings::device, true) };
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
                cout_sequence: Atomic::new(0),
                cout_running: Atomic::new(0),
                cin_sequence: Atomic::new(0),
                cin_running: Atomic::new(0),
                pd_mca1: Atomic::new(0),
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
        if *module_parameters::spkr_test.value() != 0 {
            match data.speaker_zero_dma_test(SpkrWave::Silence) {
                Ok(()) => {}
                Err(e) => dev_err!(dev, "speaker zero-DMA test failed: {:?}\n", e),
            }
        }
        if *module_parameters::spkr_props.value() != 0 {
            // Silent: attach and read the descriptors the firmware exposes.
            if data.attach_device(AUDIO_DEV_SPKR, "spkr").is_ok() {
                for m in [300u32, 301, 302, 303, 304, 305, 400, 401, 402, 403, 404, 405, 500, 501] {
                    data.dump_prop(AUDIO_DEV_SPKR, "spkr", m);
                }
            }
            for m in [301u32, 303, 304, 305] {
                data.dump_prop(AUDIO_DEV_LPAI, "lpai", m);
            }
        }
        let tone_amp = *module_parameters::spkr_tone_amp.value();
        if tone_amp != 0 {
            let tone_ms = core::cmp::min(*module_parameters::spkr_tone_ms.value(), SPKR_TONE_MS_MAX);
            let amp_ms = core::cmp::min(*module_parameters::spkr_amp_ms.value(), SPKR_AMP_MS_MAX);
            let delay_ms = *module_parameters::spkr_tone_delay_ms.value();
            if tone_amp > SPKR_TONE_AMP_MAX {
                dev_err!(dev, "spkr_tone_amp {} exceeds the ceiling {}, refusing\n",
                         tone_amp, SPKR_TONE_AMP_MAX);
            } else {
                let enc = SpkrEnc::from_param(*module_parameters::spkr_tone_enc.value());
                let wave = SpkrWave::Tone { amp: tone_amp, tone_ms, amp_ms, enc };
                let work_data = data.clone();
                let work = Arc::pin_init(
                    pin_init!(SpkrToneWork {
                        data: work_data,
                        wave,
                        work <- new_delayed_work!("SpkrToneWork::work"),
                    }),
                    GFP_KERNEL,
                )?;
                dev_info!(dev, "spkr: bounded tone (amp {}) scheduled in {} ms\n", tone_amp, delay_ms);
                let _ = workqueue::system().enqueue_delayed(work, msecs_to_jiffies(delay_ms));
            }
        }
        Ok(Self::new(data)?)
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
            description: "Bench: tone sample encoding, 0 int LE, 1 int BE, 2 float LE, 3 float BE (amp re 2^31)",
        },
    },
}

// SPDX-License-Identifier: GPL-2.0-only OR MIT
#![recursion_limit = "2048"]

//! Apple J700 (T8140) low-power microphone capture
//!
//! The AOP firmware's `lpai` service produces 16 kHz stereo S32 frames (24
//! meaningful bits) into a 512 KiB host ring mapped through dart-aop stream 9
//! and reports its cursor every 3,200 frames.  There is no DMA engine on the
//! host side: the ring is bound to the firmware over the AOP setup port and
//! the driver copies each reported span into the ALSA buffer.  Everything
//! here follows the native m1n1 host that proved the path
//! (m1n1-aurora proxyclient/m1n1/fw/aop/lpmic_probe.py).
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
    types::ForeignOwnable, //
};

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

const RATE: u32 = 16000;
const CHANNELS: u32 = 2;
const FRAME_BYTES: usize = 8;
const RING_BYTES: usize = 0x80000;
const PERIOD_FRAMES: usize = 3200;
const PERIOD_BYTES: usize = PERIOD_FRAMES * FRAME_BYTES;
const PERIODS_MAX: u32 = 16;
const REPORT_MIN_LEN: usize = 0x68;

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
    ring: Coherent<[u8]>,
    ring_bytes: Atomic<u32>,
    sequence: Atomic<u32>,
    powered: Atomic<u32>,
    hw_ptr_frames: Atomic<u32>,
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

    fn attach_lpai(&self) -> Result<()> {
        let ret = self.epic_wrapped_call(&AudioAttachDevice::new(AUDIO_DEV_LPAI))?;
        if ret != 0 {
            dev_err!(self.dev, "unable to attach lpai, return code {:#x}\n", ret);
            return Err(EIO);
        }
        Ok(())
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

    fn ring_slice(&self, off: usize, len: usize) -> &[u8] {
        // SAFETY: the ring is a coherent allocation of RING_BYTES; callers
        // keep off + len within the active ring.
        unsafe { &self.ring.as_ref()[off..off + len] }
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

#[repr(transparent)]
struct LpmicDriver(*mut bindings::snd_card);

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
        let mut this = LpmicDriver(ptr::null_mut());
        let ret = unsafe {
            bindings::snd_card_new(
                data.dev.as_raw(),
                -1,
                c_str!("NeoMic").as_char_ptr(),
                THIS_MODULE.as_ptr(),
                0,
                &mut this.0,
            )
        };
        if ret < 0 {
            dev_err!(data.dev, "Unable to allocate sound card\n");
            return Err(Error::from_errno(ret));
        }
        unsafe {
            copy_str(&mut (*this.0).driver, c"j700_lpmic".to_bytes_with_nul());
            copy_str(&mut (*this.0).id, c"NeoMic".to_bytes_with_nul());
            copy_str(&mut (*this.0).shortname, c"MacBook Neo LPMic".to_bytes_with_nul());
            copy_str(
                &mut (*this.0).longname,
                c"Apple MacBook Neo Low-Power Microphone".to_bytes_with_nul(),
            );
        }

        let mut pcm = ptr::null_mut();
        let ret = unsafe {
            bindings::snd_pcm_new(this.0, c"lpmic".as_ptr() as _, 0, 0, 1, &mut pcm)
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

        let ret = unsafe { bindings::snd_card_register(this.0) };
        if ret < 0 {
            dev_err!(data.dev, "Unable to register sound card\n");
            return Err(Error::from_errno(ret));
        }
        Ok(this)
    }
}

impl Drop for LpmicDriver {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                bindings::snd_card_free(self.0);
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

        // The source ring: 512 KiB through our own IOMMU stream (dart-aop 9).
        // SAFETY: no DMA is in flight on this device yet.
        unsafe { pdev.dma_set_mask_and_coherent(DmaMask::new::<42>())? };
        // SAFETY: the device is bound while probe runs.
        let bound = unsafe { dev.as_bound() };
        let ring = Coherent::<u8>::zeroed_slice(bound, RING_BYTES, GFP_KERNEL)?;
        let ring_iova = ring.dma_handle() as u64;

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

        data.adata.bind_source_buffer(ring_iova, RING_BYTES as u64)?;
        data.attach_lpai()?;
        data.set_channel_control()?;
        data.read_geometry()?;
        let listener: Arc<dyn ReportListener> = data.clone();
        data.adata
            .add_report_listener(svc, EPIC_SUBTYPE_PRODUCER_REPORT, listener)?;
        dev_info!(dev, "J700 low-power microphone ready (source ring at {:#x})\n", ring_iova);
        Ok(Self::new(data)?)
    }
}

module_platform_driver! {
    type: LpmicDriver,
    name: "snd_soc_j700_lpmic",
    description: "Apple J700 AOP low-power microphone capture driver",
    license: "Dual MIT/GPL",
}

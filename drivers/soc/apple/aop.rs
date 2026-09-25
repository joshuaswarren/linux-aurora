// SPDX-License-Identifier: GPL-2.0-only OR MIT
#![recursion_limit = "2048"]

//! Apple AOP driver
//!
//! Copyright (C) The Asahi Linux Contributors

use core::{
    arch::asm,
    cmp,
    mem,
    ptr,
    slice, //
};

use kernel::{
    bindings,
    c_str,
    device,
    device::{Bound, Core},
    dma::{
        Coherent,
        Device,
        DmaMask, //
    },
    error::{from_err_ptr, to_result},
    io::{
        mem::IoMem,
        Io,
        RelaxedMmio, //
    },
    iosys_map::IoSysMapRef,
    module_platform_driver,
    new_condvar,
    new_mutex,
    new_spinlock,
    of,
    platform,
    prelude::*,
    soc::apple::aop::{
        from_fourcc,
        EPICService,
        FakehidListener,
        ReportListener,
        SourceRing,
        AOP, //
    },
    soc::apple::mailbox,
    soc::apple::rtkit,
    sync::{
        aref::ARef,
        Arc,
        ArcBorrow,
        CondVar,
        CondVarTimeoutResult,
        Mutex,
        MutexGuard,
        SpinLock, //
    },
    time::msecs_to_jiffies,
    types::ForeignOwnable,
    workqueue::{
        self,
        impl_has_work,
        new_work,
        Work,
        WorkItem, //
    }, //
};

const AOP_MAX_CALLS: usize = 8;
const AOP_MMIO_SIZE: usize = 0x1e0000;
const ASC_MMIO_SIZE: usize = 0x4000;
const BOOTARGS_OFFSET: usize = 0x22c;
const BOOTARGS_SIZE: usize = 0x230;
const CPU_CONTROL: usize = 0x44;
const CPU_RUN: u32 = 0x1 << 4;
const AFK_ENDPOINT_START: u8 = 0x20;
/// 0x20..=0x2e.  This deliberately matches macOS, which was measured under the
/// hypervisor on 2026-09-20 starting exactly 0x08 (oslog), 0x20, 0x21, 0x22,
/// 0x23 and 0x2b on this AOP -- the same five AFK endpoints started here.
///
/// It is tempting to widen this: the J700 setup port sets up 0x30 and 0x32 too
/// (see SETUP_ENDPOINTS), giving them ring buffers that nothing then listens
/// on, and they looked like a candidate for the ambient light sensor's missing
/// sample path.  The trace settles it -- macOS never starts them either -- so
/// do not go there again without new evidence.
const AFK_ENDPOINT_COUNT: u8 = 0xf;
const AFK_OPC_GET_BUF: u64 = 0x89;
const AFK_OPC_INIT: u64 = 0x80;
const AFK_OPC_INIT_RX: u64 = 0x8b;
const AFK_OPC_INIT_TX: u64 = 0x8a;
const AFK_OPC_INIT_UNK: u64 = 0x8c;
const AFK_OPC_SEND: u64 = 0xa2;
const AFK_OPC_START_ACK: u64 = 0x86;
const AFK_OPC_SHUTDOWN_ACK: u64 = 0xc1;
const AFK_OPC_RECV: u64 = 0x85;
const AFK_MSG_GET_BUF_ACK: u64 = 0xa1 << 48;
const AFK_MSG_INIT: u64 = AFK_OPC_INIT << 48;
const AFK_MSG_INIT_ACK: u64 = 0xa0 << 48;
const AFK_MSG_START: u64 = 0xa3 << 48;
const AFK_MSG_SHUTDOWN: u64 = 0xc0 << 48;
const AFK_RB_BLOCK_STEP: usize = 0x40;
const EPIC_TYPE_NOTIFY: u32 = 0;
const EPIC_CATEGORY_REPORT: u8 = 0x00;
const EPIC_CATEGORY_NOTIFY: u8 = 0x10;
const EPIC_CATEGORY_REPLY: u8 = 0x20;
const EPIC_SUBTYPE_STD_SERVICE: u16 = 0xc0;
const EPIC_SUBTYPE_FAKEHID_REPORT: u16 = 0xc4;
const EPIC_SUBTYPE_RETCODE: u16 = 0x84;
const EPIC_SUBTYPE_RETCODE_PAYLOAD: u16 = 0xa0;
const EPIC_SUBTYPE_STRING: u16 = 0x8a;
const QE_MAGIC1: u32 = from_fourcc(b" POI");
const QE_MAGIC2: u32 = from_fourcc(b" POA");

// T8140 (J700) AOP "setup port": the second mailbox the firmware boots
// through.  Everything below was measured by the native m1n1 microphone host
// (m1n1-aurora proxyclient/m1n1/fw/aop/j700_lpmic_native.py, RESULTS in
// artifacts/j700-native-lpmic-20260906).
const SETUP_PAGE: usize = 0x4000;
const SETUP_ARENA_PAGES: usize = 13;
const SETUP_MGMT_EP: u8 = 0;
const SETUP_TYPE_HELLO: u64 = 1;
const SETUP_TYPE_HELLO_REPLY: u64 = 2;
const SETUP_TYPE_UNK3: u64 = 3;
const SETUP_TYPE_UNK3_REPLY: u64 = 4;
const SETUP_TYPE_PWR_ACK: u64 = 7;
const SETUP_TYPE_EPMAP: u64 = 8;
const SETUP_TYPE_AP_PWR: u64 = 0xb;
const SETUP_HELLO_VERSION: u64 = 0xc000c;
const SETUP_EPMAP_LAST: u64 = 1 << 51;
const SETUP_AP_PWR_INIT: u64 = 0x220;
const SETUP_AP_PWR_ON: u64 = 0x20;
const SETUP_BUFFER_REQUEST: u64 = 0x12;
const SETUP_BUFFER_REQUEST_ACK: u64 = 0x1200000000000000;
const SETUP_ENDPOINTS: [u8; 5] = [0x20, 0x21, 0x23, 0x30, 0x32];
const SETUP_BOOT_TIMEOUT_MS: i64 = 15000;
const SETUP_SOURCE_EP: u8 = 0x20;
/// The ALS lives on setup-port endpoint 0x21, and the trusted side will not
/// power the CT817 until its calibration has been pushed there.
const SETUP_ALS_EP: u8 = 0x21;
/// The calibration message, captured verbatim from the exclave's own
/// setup-port traffic on 2026-09-21 (see ALS-STATE-2026-09-20.md).  Eighty
/// bytes on the wire: operation 0x0746b8d665152e31, an inner length of 56,
/// then fourteen float32 -- five channel coefficients and two
/// (1, gain, ?, max) triplets.  These are this part's numbers; they come from
/// the machine, not from us, which is why they are carried verbatim.
const SETUP_ALS_CALIBRATION: [u8; 80] = [
    0x31, 0x2e, 0x15, 0x65, 0xd6, 0xb8, 0x46, 0x07, 0x38, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xe4, 0x9e, 0xe3, 0x3e, 0xaf, 0xc0, 0xae, 0x3f, 0xb7, 0xe8, 0x36, 0x3e, 0x96, 0xc0, 0x95, 0x3e,
    0x84, 0x89, 0x03, 0x3f, 0x00, 0x00, 0x80, 0x3f, 0x00, 0x00, 0x80, 0x3f, 0x9d, 0x9c, 0xfc, 0x40,
    0xb1, 0xb0, 0x80, 0x41, 0x72, 0x72, 0xfa, 0x42, 0x00, 0x00, 0x80, 0x3f, 0x9e, 0x9d, 0xfd, 0x40,
    0xe2, 0xe1, 0x81, 0x41, 0x78, 0x78, 0xfe, 0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
/// The AOP answers the calibration with 0x2000000000000004 rather than the
/// 0x2000000000000001 the source bind gets.
const SETUP_REPLY_READY_ALT: u64 = 0x2000000000000004;
const SETUP_SET_SOURCE_BUFFER: u64 = 0x6b803ce492bd9547;
const SETUP_REQUEST_TYPE: u64 = 3 << 60;
const SETUP_REQUEST_DONE: u64 = 4 << 60;
const SETUP_REPLY_READY: u64 = 0x2000000000000001;
const SETUP_REPLY_TIMEOUT_MS: i64 = 5000;
/// Bound on an EPIC call: the firmware can stop answering (e.g. a power
/// request against a fabric the AP turned off), and the caller must not be
/// wedged in D state forever.
const EPIC_CALL_TIMEOUT_MS: i64 = 5000;

fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

/// Index into the AFK endpoint tables for RTKit endpoint `ep`, if it is one
/// of the endpoints this driver drives.
fn afk_endpoint_index(ep: u8) -> Option<usize> {
    ep.checked_sub(AFK_ENDPOINT_START)
        .filter(|i| *i < AFK_ENDPOINT_COUNT)
        .map(usize::from)
}

/// Reads the little-endian `u16` at `off`; `b` must hold it.
fn le_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

/// Reads the little-endian `u32` at `off`; `b` must hold it.
fn le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline(always)]
fn mem_sync() {
    unsafe {
        asm!("dsb sy");
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct QEHeader {
    magic: u32,
    size: u32,
    channel: u32,
    ty: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct EPICHeader {
    version: u8,
    seq: u16,
    _pad0: u8,
    _unk0: u32,
    timestamp: u64,
    // Subheader
    length: u32,
    sub_version: u8,
    category: u8,
    subtype: u16,
    tag: u16,
    _unk1: u16,
    _pad1: u64,
    inline_len: u32,
}

/// The EPIC header fields that message dispatch needs.
struct EPICHeaderFields {
    category: u8,
    subtype: u16,
    tag: u16,
}

impl EPICHeaderFields {
    /// Splits `msg` into its decoded header and its payload, or returns `None`
    /// when `msg` is too short to hold a header.
    fn decode(msg: &[u8]) -> Option<(EPICHeaderFields, &[u8])> {
        if msg.len() < mem::size_of::<EPICHeader>() {
            return None;
        }
        let (hdr, data) = msg.split_at(mem::size_of::<EPICHeader>());
        let fields = EPICHeaderFields {
            category: hdr[mem::offset_of!(EPICHeader, category)],
            subtype: le_u16(hdr, mem::offset_of!(EPICHeader, subtype)),
            tag: le_u16(hdr, mem::offset_of!(EPICHeader, tag)),
        };
        Some((fields, data))
    }
}

/// A service announcement (report subtype [`EPIC_SUBTYPE_STD_SERVICE`]):
/// a NUL-padded 32-byte service name followed by the channel the service
/// answers on. Its trailing 8 bytes are not used here.
const EPIC_ANNOUNCE_NAME_LEN: usize = 32;
const EPIC_ANNOUNCE_CHANNEL_OFFSET: usize = 32;
const EPIC_ANNOUNCE_LEN: usize = 44;

#[pin_data]
struct FutureValue<T> {
    #[pin]
    val: Mutex<Option<T>>,
    #[pin]
    completion: CondVar,
}

impl<T> FutureValue<T> {
    fn pin_init() -> impl PinInit<FutureValue<T>> {
        pin_init!(
            FutureValue {
                val <- new_mutex!(None),
                completion <- new_condvar!()
            }
        )
    }
    fn complete(&self, val: T) {
        *self.val.lock() = Some(val);
        self.completion.notify_all();
    }
    fn wait(&self) -> T {
        let mut ret_guard = self.val.lock();
        while ret_guard.is_none() {
            self.completion.wait(&mut ret_guard);
        }
        ret_guard.take().unwrap()
    }
    /// Waits at most `timeout_ms`; `None` when the value never arrived.
    ///
    /// The wait is uninterruptible: a firmware transaction must not be
    /// abandoned because the calling task has a signal pending (a daemon
    /// closing its PCM on SIGTERM), or its prompt reply would arrive for a
    /// call that no longer exists.
    fn wait_timeout(&self, timeout_ms: i64) -> Option<T> {
        let mut ret_guard = self.val.lock();
        let mut left = msecs_to_jiffies(timeout_ms as u32);
        while ret_guard.is_none() {
            match self.completion.wait_timeout(&mut ret_guard, left) {
                CondVarTimeoutResult::Timeout => return ret_guard.take(),
                CondVarTimeoutResult::Woken { jiffies }
                | CondVarTimeoutResult::Signal { jiffies } => {
                    left = jiffies;
                }
            }
        }
        ret_guard.take()
    }
    fn reset(&self) {
        *self.val.lock() = None;
    }
}

struct AFKRingBuffer {
    offset: usize,
    block_size: usize,
    buf_size: usize,
}

struct CallResult {
    retcode: u32,
    extra_data: Option<KVec<u8>>,
}

struct AFKEndpoint {
    index: u8,
    iomem: Option<Coherent<[u8]>>,
    txbuf: Option<AFKRingBuffer>,
    rxbuf: Option<AFKRingBuffer>,
    seq: u16,
    calls: [Option<Arc<FutureValue<CallResult>>>; AOP_MAX_CALLS],
    call_returns: [Option<KVec<u8>>; AOP_MAX_CALLS],
    /// Reports seen on this endpoint, so the log records that they arrive
    /// without one line every 200 ms for the life of the boot.
    reports: u32,
}

unsafe impl Send for AFKEndpoint {}

impl AFKEndpoint {
    fn new(index: u8) -> AFKEndpoint {
        AFKEndpoint {
            index,
            iomem: None,
            txbuf: None,
            rxbuf: None,
            seq: 0,
            calls: [const { None }; AOP_MAX_CALLS],
            call_returns: [const { None }; AOP_MAX_CALLS],
            reports: 0,
        }
    }

    fn start(&self, rtkit: Pin<&mut rtkit::RtKit<AopData>>) -> Result<()> {
        rtkit.send_message(self.index, AFK_MSG_INIT)
    }

    fn stop(&self, rtkit: Pin<&mut rtkit::RtKit<AopData>>) -> Result<()> {
        rtkit.send_message(self.index, AFK_MSG_SHUTDOWN)
    }

    fn recv_message(
        &mut self,
        client: ArcBorrow<'_, AopData>,
        rtkit: Pin<&mut rtkit::RtKit<AopData>>,
        msg: u64,
    ) -> Result<()> {
        let opc = msg >> 48;
        match opc {
            AFK_OPC_INIT => {
                rtkit.send_message(self.index, AFK_MSG_INIT_ACK)?;
            }
            AFK_OPC_GET_BUF => {
                self.recv_get_buf(client.dev.clone(), rtkit, msg)?;
            }
            AFK_OPC_INIT_UNK => {} // no-op
            AFK_OPC_START_ACK => {}
            AFK_OPC_INIT_RX => {
                if self.rxbuf.is_some() {
                    dev_err!(
                        client.dev,
                        "Got InitRX message with existing rxbuf at endpoint {}",
                        self.index
                    );
                    return Err(EIO);
                }
                self.rxbuf = Some(self.parse_ring_buf(msg)?);
                if self.txbuf.is_some() {
                    rtkit.send_message(self.index, AFK_MSG_START)?;
                }
            }
            AFK_OPC_INIT_TX => {
                if self.txbuf.is_some() {
                    dev_err!(
                        client.dev,
                        "Got InitTX message with existing txbuf at endpoint {}",
                        self.index
                    );
                    return Err(EIO);
                }
                self.txbuf = Some(self.parse_ring_buf(msg)?);
                if self.rxbuf.is_some() {
                    rtkit.send_message(self.index, AFK_MSG_START)?;
                }
            }
            AFK_OPC_RECV => {
                self.recv_rb(client)?;
            }
            AFK_OPC_SHUTDOWN_ACK => {
                client.shutdown_complete();
            }
            _ => dev_err!(
                client.dev,
                "AFK endpoint {} got unknown message {}",
                self.index,
                msg
            ),
        }
        Ok(())
    }

    /// Decodes an InitRX/InitTX message: a ring of `size` bytes at `offset`
    /// in the shared buffer, laid out as three header blocks (buffer size,
    /// read pointer, write pointer) followed by `buf_size` bytes of entries.
    fn parse_ring_buf(&self, msg: u64) -> Result<AFKRingBuffer> {
        let msg = msg as usize;
        let size = ((msg >> 16) & 0xFFFF) * AFK_RB_BLOCK_STEP;
        let offset = ((msg >> 32) & 0xFFFF) * AFK_RB_BLOCK_STEP;
        let iomem_size = self.iomem.as_ref().ok_or(ENXIO)?.size();
        let buf_size = self.iomem_read32(offset)? as usize;
        let block_size = size.checked_sub(buf_size).ok_or(EIO)? / 3;
        let end = offset.checked_add(size).ok_or(EIO)?;
        if end > iomem_size || buf_size == 0 || block_size == 0 || !block_size.is_power_of_two() {
            return Err(EIO);
        }
        Ok(AFKRingBuffer {
            offset,
            block_size,
            buf_size,
        })
    }

    /// Returns a pointer to `len` bytes at `off` in the shared buffer after
    /// checking that they lie inside it.
    fn iomem_ptr(&self, off: usize, len: usize) -> Result<*mut u8> {
        let iomem = self.iomem.as_ref().ok_or(ENXIO)?;
        let end = off.checked_add(len).ok_or(EIO)?;
        if end > iomem.size() {
            return Err(EIO);
        }
        // SAFETY: `off + len` does not exceed the size of the allocation, so
        // the offset pointer stays inside it.
        Ok(unsafe { iomem.as_mut_ptr().cast::<u8>().add(off) })
    }

    /// Writes one of the ring's pointer words. The firmware polls the word
    /// concurrently, so the write is volatile.
    fn iomem_write32(&mut self, off: usize, data: u32) -> Result<()> {
        if off % mem::align_of::<u32>() != 0 {
            return Err(EIO);
        }
        let ptr = self.iomem_ptr(off, mem::size_of::<u32>())?;
        // SAFETY: `ptr` is valid for four bytes and aligned for a `u32`. A
        // volatile write is the kernel's WRITE_ONCE() for a word the device
        // reads at any time.
        unsafe { ptr.cast::<u32>().write_volatile(data.to_le()) };
        Ok(())
    }

    /// Reads one of the ring's pointer words. The firmware updates the word
    /// concurrently, so the read is volatile.
    fn iomem_read32(&self, off: usize) -> Result<u32> {
        if off % mem::align_of::<u32>() != 0 {
            return Err(EIO);
        }
        let ptr = self.iomem_ptr(off, mem::size_of::<u32>())?;
        // SAFETY: `ptr` is valid for four bytes and aligned for a `u32`. A
        // volatile read is the kernel's READ_ONCE() for a word the device
        // writes at any time.
        Ok(u32::from_le(unsafe { ptr.cast::<u32>().read_volatile() }))
    }

    /// Copies a ring entry out of the shared buffer. Callers only read
    /// entries between the read and the write pointer, which the firmware
    /// has finished writing and does not touch again until the read pointer
    /// passes them.
    fn memcpy_from_iomem(&self, off: usize, target: &mut [u8]) -> Result<()> {
        let src = self.iomem_ptr(off, target.len())?;
        // SAFETY: `src` is valid for `target.len()` bytes, `target` is a
        // distinct allocation, and by the ring protocol (see above) the device
        // does not write the entry while it is copied.
        unsafe { ptr::copy_nonoverlapping(src, target.as_mut_ptr(), target.len()) };
        Ok(())
    }

    /// Copies a ring entry into the shared buffer. Callers only write between
    /// the write and the read pointer, which the firmware does not read until
    /// the write pointer is advanced past the entry.
    fn memcpy_to_iomem(&mut self, off: usize, src: &[u8]) -> Result<()> {
        let dst = self.iomem_ptr(off, src.len())?;
        // SAFETY: `dst` is valid for `src.len()` bytes, `src` is a distinct
        // allocation, and by the ring protocol (see above) the device does
        // not access the entry while it is written.
        unsafe { ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
        Ok(())
    }

    fn recv_get_buf(
        &mut self,
        dev: ARef<device::Device>,
        rtkit: Pin<&mut rtkit::RtKit<AopData>>,
        msg: u64,
    ) -> Result<()> {
        let size = ((msg & 0xFFFF0000) >> 16) as usize * AFK_RB_BLOCK_STEP;
        if self.iomem.is_some() {
            dev_err!(
                dev,
                "Got GetBuf message with existing buffer on endpoint {}",
                self.index
            );
            return Err(EIO);
        }
        // SAFETY: TODO
        let bound_dev = unsafe { dev.as_bound() };
        let iomem = Coherent::<u8>::zeroed_slice(bound_dev, size, GFP_KERNEL)?;
        rtkit.send_message(self.index, AFK_MSG_GET_BUF_ACK | iomem.dma_handle())?;
        self.iomem = Some(iomem);
        Ok(())
    }

    fn recv_rb(&mut self, client: ArcBorrow<'_, AopData>) -> Result<()> {
        let (buf_offset, block_size, buf_size) = match self.rxbuf.as_ref() {
            Some(b) => (b.offset, b.block_size, b.buf_size),
            None => {
                dev_err!(
                    client.dev,
                    "Got Recv message with no rxbuf at endpoint {}",
                    self.index
                );
                return Err(EIO);
            }
        };
        let mut rptr = self.iomem_read32(buf_offset + block_size)? as usize;
        let mut wptr = self.iomem_read32(buf_offset + block_size * 2)?;
        mem_sync();
        let base = buf_offset + block_size * 3;
        let mut msg_buf = KVec::new();
        const QEH_SIZE: usize = mem::size_of::<QEHeader>();
        while wptr as usize != rptr {
            let mut qeh_bytes = [0; QEH_SIZE];
            self.memcpy_from_iomem(base + rptr, &mut qeh_bytes)?;
            let mut qeh = unsafe { &*(qeh_bytes.as_ptr() as *const QEHeader) };
            if qeh.magic != QE_MAGIC1 && qeh.magic != QE_MAGIC2 {
                let magic = qeh.magic;
                dev_err!(
                    client.dev,
                    "Invalid magic on ep {}, got {:x}",
                    self.index,
                    magic
                );
                return Err(EIO);
            }
            let room = buf_size.checked_sub(rptr + QEH_SIZE).ok_or(EIO)?;
            if qeh.size as usize > room {
                rptr = 0;
                self.memcpy_from_iomem(base + rptr, &mut qeh_bytes)?;
                qeh = unsafe { &*(qeh_bytes.as_ptr() as *const QEHeader) };

                if qeh.magic != QE_MAGIC1 && qeh.magic != QE_MAGIC2 {
                    let magic = qeh.magic;
                    dev_err!(
                        client.dev,
                        "Invalid magic on ep {}, got {:x}",
                        self.index,
                        magic
                    );
                    return Err(EIO);
                }
            }
            msg_buf.resize(qeh.size as usize, 0, GFP_KERNEL)?;
            self.memcpy_from_iomem(base + rptr + QEH_SIZE, &mut msg_buf)?;
            let Some((header, msg)) = EPICHeaderFields::decode(&msg_buf) else {
                dev_err!(
                    client.dev,
                    "Short EPIC message ({} bytes) on ep {}",
                    msg_buf.len(),
                    self.index
                );
                return Err(EIO);
            };
            self.handle_ipc(client, qeh, &header, msg)?;
            rptr = align_up(rptr + QEH_SIZE + qeh.size as usize, block_size) % buf_size;
            mem_sync();
            self.iomem_write32(buf_offset + block_size, rptr as u32)?;
            wptr = self.iomem_read32(buf_offset + block_size * 2)?;
            mem_sync();
        }
        Ok(())
    }
    fn handle_ipc(
        &mut self,
        client: ArcBorrow<'_, AopData>,
        qhdr: &QEHeader,
        ehdr: &EPICHeaderFields,
        data: &[u8],
    ) -> Result<()> {
        let subtype = ehdr.subtype;
        if ehdr.category == EPIC_CATEGORY_REPORT {
            // Unconditional: the question "does the AOP ever send a report at
            // all, on any endpoint or channel?" has to be answerable without
            // depending on a module parameter.  aop.report_trace could not be
            // read back from /sys/module/aop/parameters on this build, so a
            // silent log left it untestable.
            //
            // The first report on the endpoint answers that question; the ALS
            // then streams one every 200 ms, and logging each wrapped the
            // kernel ring every couple of minutes and destroyed every other
            // driver's boot log.  Say it once and count the rest.
            // QEHeader is repr(C, packed): bind the field to a local before
            // formatting, or rustc rejects the unaligned reference (E0793).
            let rep_ch = qhdr.channel;
            let rep_ep = self.index;
            let rep_len = data.len();
            self.reports = self.reports.saturating_add(1);
            if self.reports == 1 {
                dev_info!(
                    client.dev,
                    "aop: EPIC report ep {:#04x} channel {:#x} subtype {:#06x} ({} bytes); further reports on this endpoint are not logged\n",
                    rep_ep,
                    rep_ch,
                    subtype,
                    rep_len
                );
            }
            if subtype == EPIC_SUBTYPE_STD_SERVICE {
                if data.len() < EPIC_ANNOUNCE_LEN {
                    dev_err!(
                        client.dev,
                        "Short service announcement ({} bytes) on endpoint {}",
                        data.len(),
                        self.index
                    );
                    return Err(EIO);
                }
                let name = &data[..EPIC_ANNOUNCE_NAME_LEN];
                let name = &name[..name.iter().position(|x| *x == 0).unwrap_or(name.len())];
                let chan = le_u32(data, EPIC_ANNOUNCE_CHANNEL_OFFSET);
                return Into::<Arc<_>>::into(client).register_service(self, chan, name);
            } else if subtype == EPIC_SUBTYPE_FAKEHID_REPORT {
                return client.process_fakehid_report(self, qhdr.channel, data);
            } else if client.process_report(self, qhdr.channel, subtype, data)? {
                return Ok(());
            } else {
                dev_err!(
                    client.dev,
                    "Unexpected EPIC report subtype {:x} on endpoint {}",
                    subtype,
                    self.index
                );
                return Err(EIO);
            }
        } else if ehdr.category == EPIC_CATEGORY_REPLY {
            if subtype == EPIC_SUBTYPE_RETCODE_PAYLOAD
                || subtype == EPIC_SUBTYPE_RETCODE
                || subtype == EPIC_SUBTYPE_STRING
            {
                if data.len() < mem::size_of::<u32>() {
                    dev_err!(
                        client.dev,
                        "Retcode data too short on endpoint {}",
                        self.index
                    );
                    return Err(EIO);
                }
                let retcode = le_u32(data, 0);
                let tag = ehdr.tag as usize;
                if tag == 0 || tag > self.calls.len() || self.calls[tag - 1].is_none() {
                    dev_err!(
                        client.dev,
                        "Dropping a retcode with no call in flight (tag {:?}) on endpoint {}",
                        tag,
                        self.index
                    );
                    return Ok(());
                }
                let future = self.calls[tag - 1].take().unwrap();
                let extra_data = if let Some(mut ret) = self.call_returns[tag - 1].take() {
                    let len = cmp::min(data.len() - 4, ret.len());
                    ret[..len].copy_from_slice(&data[4..(len + 4)]);
                    ret.truncate(len);
                    Some(ret)
                } else {
                    None
                };
                future.complete(CallResult {
                    retcode,
                    extra_data,
                });

                return Ok(());
            } else {
                dev_err!(
                    client.dev,
                    "Unexpected EPIC reply subtype {:x} on endpoint {}",
                    subtype,
                    self.index
                );
                return Err(EIO);
            }
        }
        dev_err!(
            client.dev,
            "Unexpected EPIC category {:x} on endpoint {}",
            ehdr.category,
            self.index
        );
        Err(EIO)
    }
    fn send_rb(
        &mut self,
        client: &AopData,
        rtkit: Pin<&mut rtkit::RtKit<AopData>>,
        channel: u32,
        ty: u32,
        header: &[u8],
        data: &[u8],
    ) -> Result<()> {
        let (buf_offset, block_size, buf_size) = match self.txbuf.as_ref() {
            Some(b) => (b.offset, b.block_size, b.buf_size),
            None => {
                dev_err!(
                    client.dev,
                    "Attempting to send message with no txbuf at endpoint {}",
                    self.index
                );
                return Err(EIO);
            }
        };
        let base = buf_offset + block_size * 3;
        mem_sync();
        let rptr = self.iomem_read32(buf_offset + block_size)? as usize;
        let mut wptr = self.iomem_read32(buf_offset + block_size * 2)? as usize;
        const QEH_SIZE: usize = mem::size_of::<QEHeader>();
        if wptr < rptr && wptr + QEH_SIZE >= rptr {
            dev_err!(client.dev, "Tx buffer full at endpoint {}", self.index);
            return Err(EIO);
        }
        let payload_len = header.len() + data.len();
        let qeh = QEHeader {
            magic: QE_MAGIC1,
            size: payload_len as u32,
            channel,
            ty,
        };
        let qeh_bytes = unsafe {
            slice::from_raw_parts(
                &qeh as *const QEHeader as *const u8,
                mem::size_of::<QEHeader>(),
            )
        };
        self.memcpy_to_iomem(base + wptr, qeh_bytes)?;
        if payload_len > buf_size.checked_sub(wptr + QEH_SIZE).ok_or(EIO)? {
            wptr = 0;
            self.memcpy_to_iomem(base + wptr, qeh_bytes)?;
        }
        self.memcpy_to_iomem(base + wptr + QEH_SIZE, header)?;
        self.memcpy_to_iomem(base + wptr + QEH_SIZE + header.len(), data)?;
        wptr = align_up(wptr + QEH_SIZE + payload_len, block_size) % buf_size;
        self.iomem_write32(buf_offset + block_size * 2, wptr as u32)?;
        let msg = wptr as u64 | (AFK_OPC_SEND << 48);
        rtkit.send_message(self.index, msg)
    }
    fn epic_notify(
        &mut self,
        client: &AopData,
        rtkit: Pin<&mut rtkit::RtKit<AopData>>,
        channel: u32,
        subtype: u16,
        category: u8,
        data: &[u8],
        ret: Option<KVec<u8>>,
    ) -> Result<Arc<FutureValue<CallResult>>> {
        let mut tag = 0;
        for i in 0..self.calls.len() {
            if self.calls[i].is_none() {
                tag = i + 1;
                break;
            }
        }
        if tag == 0 {
            dev_err!(
                client.dev,
                "Too many inflight calls on endpoint {}",
                self.index
            );
            return Err(EIO);
        }
        let call = Arc::pin_init(FutureValue::pin_init(), GFP_KERNEL)?;
        // 2026-09-20: this field was declared and never set, so every call this
        // driver has ever made advertised a reply capacity of zero.  macOS sets
        // it on every call -- 0xe4 for the ALS HID-descriptor fetch (subtype
        // 0x01), and 2 / 4 / 0x40 matching each property's width for the
        // property reads.  Property gets happen to be answered anyway with a
        // zero here, which is why it went unnoticed; do not assume the same of
        // a call the firmware has never been asked by this driver before.
        let inline_len = ret.as_ref().map(|b| b.len() as u32).unwrap_or(0);
        let hdr_v2 = EPICHeader {
            version: 2,
            seq: self.seq,
            length: data.len() as u32,
            sub_version: 2,
            category,
            subtype,
            tag: tag as u16,
            inline_len,
            ..EPICHeader::default()
        };
        let hdr_v4 = EPICHeaderV4 {
            version: 2,
            seq: self.seq,
            length: data.len() as u32,
            sub_version: 4,
            category,
            subtype,
            tag: tag as u16,
            inline_len,
            ..EPICHeaderV4::default()
        };
        self.call_returns[tag - 1] = ret;
        // macOS speaks v2 to this firmware; see the epic_tx_v2 parameter.
        let tx_v4 = client.epic_v4;
        let hdr_bytes = if tx_v4 {
            // SAFETY: `hdr_v4` is a packed, plain-data header that lives
            // for the whole call; the slice covers exactly its bytes.
            unsafe {
                slice::from_raw_parts(
                    &hdr_v4 as *const EPICHeaderV4 as *const u8,
                    mem::size_of::<EPICHeaderV4>(),
                )
            }
        } else {
            unsafe {
                slice::from_raw_parts(
                    &hdr_v2 as *const EPICHeader as *const u8,
                    mem::size_of::<EPICHeader>(),
                )
            }
        };

        self.send_rb(client, rtkit, channel, EPIC_TYPE_NOTIFY, hdr_bytes, data)?;
        self.seq = self.seq.wrapping_add(1);
        self.calls[tag - 1] = Some(call.clone());
        Ok(call)
    }
    /// Drops a call that will never be waited on again (timed out).
    fn cancel_call(&mut self, call: &Arc<FutureValue<CallResult>>) {
        for i in 0..self.calls.len() {
            if let Some(c) = &self.calls[i] {
                if Arc::ptr_eq(c, call) {
                    self.calls[i] = None;
                    self.call_returns[i] = None;
                    return;
                }
            }
        }
    }
}

struct ListenerEntry {
    svc: EPICService,
    listener: Arc<dyn FakehidListener>,
}

#[pin_data]
struct AopData {
    dev: ARef<device::Device>,
    epic_v4: bool,
    #[pin]
    setup: SpinLock<SetupState>,
    #[pin]
    setup_cv: CondVar,
    #[pin]
    rtkit: Mutex<Option<rtkit::RtKit<AopData>>>,
    #[pin]
    endpoints: [Mutex<AFKEndpoint>; AFK_ENDPOINT_COUNT as usize],
    /// The EPIC v4 firmware answers without the request tag, so an endpoint
    /// carries one call at a time: callers queue here for their turn.
    #[pin]
    call_turn: [Mutex<()>; AFK_ENDPOINT_COUNT as usize],
    #[pin]
    ep_shutdown: FutureValue<()>,
    #[pin]
    hid_listeners: Mutex<KVec<ListenerEntry>>,
    #[pin]
    report_listeners: Mutex<KVec<ReportListenerEntry>>,
    #[pin]
    subdevices: Mutex<KVec<*mut bindings::platform_device>>,
    #[pin]
    source_ring: Mutex<Option<Arc<SourceRing>>>,
}

unsafe impl Send for AopData {}
unsafe impl Sync for AopData {}

#[pin_data]
struct AopServiceRegisterWork {
    name: &'static CStr,
    data: Arc<AopData>,
    service: EPICService,
    #[pin]
    work: Work<AopServiceRegisterWork>,
}

impl_has_work! {
    impl HasWork<Self, 0> for AopServiceRegisterWork { self.work }
}

impl AopServiceRegisterWork {
    fn new(
        name: &'static CStr,
        data: Arc<AopData>,
        service: EPICService,
    ) -> Result<Pin<KBox<Self>>> {
        KBox::pin_init(
            pin_init!(AopServiceRegisterWork {
                name, data, service,
                work <- new_work!("AopServiceRegisterWork::work"),
            }),
            GFP_KERNEL,
        )
    }
}

impl WorkItem for AopServiceRegisterWork {
    type Pointer = Pin<KBox<AopServiceRegisterWork>>;

    fn run(this: Pin<KBox<AopServiceRegisterWork>>) {
        // Held until the device has been registered: the registration takes
        // its own reference on the node only then.
        let fwnode = this
            .data
            .dev
            .fwnode()
            .and_then(|x| x.get_child_by_name(this.name));
        let info = bindings::platform_device_info {
            parent: this.data.dev.as_raw(),
            name: this.name.as_ptr() as *const _,
            id: bindings::PLATFORM_DEVID_AUTO,
            res: ptr::null_mut(),
            num_res: 0,
            data: &this.service as *const EPICService as *const _,
            size_data: mem::size_of::<EPICService>(),
            dma_mask: 0,
            fwnode: fwnode
                .as_ref()
                .map(|x| x.as_raw())
                .unwrap_or(ptr::null_mut()),
            swnode: ptr::null_mut(),
            properties: ptr::null_mut(),
            of_node_reused: false,
        };
        let pdev = unsafe { from_err_ptr(bindings::platform_device_register_full(&info)) };
        drop(fwnode);
        match pdev {
            Err(e) => {
                dev_err!(
                    this.data.dev,
                    "Failed to create device for service {:?}: {:?}",
                    this.name,
                    e
                );
            }
            Ok(pdev) => {
                let res = this.data.subdevices.lock().push(pdev, GFP_KERNEL);
                if res.is_err() {
                    dev_err!(this.data.dev, "Failed to store subdevice");
                }
            }
        }
    }
}

impl AopData {
    fn new(dev: &platform::Device<Core>, epic_v4: bool) -> Result<Arc<AopData>> {
        Arc::pin_init(
            pin_init!(
                AopData {
                    dev: dev.as_ref().into(),
                    epic_v4,
                    setup <- new_spinlock!(SetupState::new()),
                    setup_cv <- new_condvar!(),
                    rtkit <- new_mutex!(None),
                    endpoints <- pin_init::pin_init_array_from_fn(|i| {
                        new_mutex!(AFKEndpoint::new(AFK_ENDPOINT_START + i as u8))
                    }),
                    call_turn <- pin_init::pin_init_array_from_fn(|_| new_mutex!(())),
                    ep_shutdown <- FutureValue::pin_init(),
                    hid_listeners <- new_mutex!(KVec::new()),
                    report_listeners <- new_mutex!(KVec::new()),
                    subdevices <- new_mutex!(KVec::new()),
                    source_ring <- new_mutex!(None),
                }
            ),
            GFP_KERNEL,
        )
    }
    fn start(&self) -> Result<()> {
        {
            let mut guard = self.rtkit.lock();
            let mut rtk = guard.as_mut().as_pin_mut().unwrap();
            rtk.as_mut().wake()?;
        }
        self.start_afk()
    }
    fn start_afk(&self) -> Result<()> {
        for ep in 0..AFK_ENDPOINT_COUNT {
            let rtk_ep_num = AFK_ENDPOINT_START + ep;
            let mut guard = self.rtkit.lock();
            let mut rtk = guard.as_mut().as_pin_mut().unwrap();
            if !rtk.as_mut().has_endpoint(rtk_ep_num) {
                continue;
            }
            // The J700 application map is 0x20 misc, 0x21 aop-audio,
            // 0x22 aop-voicetrigger, 0x23 als, 0x2b aop-audprov.  The firmware
            // blocks its own startup until every advertised endpoint is
            // enabled, so record which ones are actually present here.
            dev_info!(self.dev, "AFK: starting endpoint {:#04x}\n", rtk_ep_num);
            // The five endpoints of the proven J700 application map stay fatal:
            // audio depends on 0x21/0x2b and probe should not come up half
            // working.  0x30/0x32 only became reachable when AFK_ENDPOINT_COUNT
            // was widened and nothing has ever spoken to them, so if one will
            // not start, say so and carry on rather than failing the whole AOP
            // probe and taking the speakers down to test a hypothesis.
            let required = matches!(rtk_ep_num, 0x20 | 0x21 | 0x22 | 0x23 | 0x2b);
            if let Err(e) = rtk.as_mut().start_endpoint(rtk_ep_num) {
                if required {
                    return Err(e);
                }
                dev_warn!(
                    self.dev,
                    "AFK: endpoint {:#04x} did not start ({:?}), skipping\n",
                    rtk_ep_num,
                    e
                );
                continue;
            }
            let ep_guard = self.endpoints[ep as usize].lock();
            if let Err(e) = ep_guard.start(rtk.as_mut()) {
                if required {
                    return Err(e);
                }
                dev_warn!(
                    self.dev,
                    "AFK: endpoint {:#04x} AFK init failed ({:?}), skipping\n",
                    rtk_ep_num,
                    e
                );
            }
        }
        Ok(())
    }
    fn register_service(
        self: Arc<Self>,
        ep: &mut AFKEndpoint,
        channel: u32,
        name: &[u8],
    ) -> Result<()> {
        let svc = EPICService {
            channel,
            endpoint: ep.index,
        };
        // Every announcement is logged, including the ones with no sub-device.
        // On the J700 endpoint 0x20 announces SPUApp / SPUAppUntrusted /
        // HibernationAppUntrusted and all three used to be dropped in silence,
        // which hid the fact that the ambient light sensor is a CT817 HID
        // device on the SPU (macOS: AppleSPUCT817 -> IOHIDEventService ->
        // AppleSPUALSDriver::eventServiceCallback) rather than something the
        // `als` EPIC service streams.
        dev_info!(
            self.dev,
            "aop: service announce ep {:#04x} channel {:#x} name {}\n",
            ep.index,
            channel,
            core::str::from_utf8(name).unwrap_or("<non-utf8>")
        );
        let dev_name = match name {
            b"aop-audio" => c_str!("audio"),
            b"las" => c_str!("las"),
            b"als" => c_str!("als"),
            // The SPU application endpoint.  On the J700 the ambient light
            // sensor is a CT817 HID device reached through here, not through
            // the `als` EPIC service (which answers property reads and then
            // never reports).  Create the sub-devices so a driver can bind and
            // claim the channel; HibernationAppUntrusted stays dropped.
            b"SPUApp" => c_str!("spu"),
            b"SPUAppUntrusted" => c_str!("spu-untrusted"),
            _ => {
                return Ok(());
            }
        };
        // probe can call back into us, run it with locks dropped.
        let work = AopServiceRegisterWork::new(dev_name, self, svc)?;
        workqueue::system().enqueue(work);
        Ok(())
    }

    /// Returns Ok(true) when a registered listener took the report.
    fn process_report(&self, ep: &AFKEndpoint, ch: u32, subtype: u16, data: &[u8]) -> Result<bool> {
        let guard = self.report_listeners.lock();
        for entry in &*guard {
            if entry.svc.endpoint == ep.index && entry.svc.channel == ch && entry.subtype == subtype
            {
                entry.listener.process_report(subtype, data)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn process_fakehid_report(&self, ep: &AFKEndpoint, ch: u32, data: &[u8]) -> Result<()> {
        let guard = self.hid_listeners.lock();
        for entry in &*guard {
            if entry.svc.endpoint == ep.index && entry.svc.channel == ch {
                return entry.listener.process_fakehid_report(data);
            }
        }
        Ok(())
    }

    fn shutdown_complete(&self) {
        self.ep_shutdown.complete(());
    }

    fn stop(&self) -> Result<()> {
        for ep in 0..AFK_ENDPOINT_COUNT {
            {
                let rtk_ep_num = AFK_ENDPOINT_START + ep;
                let mut guard = self.rtkit.lock();
                let mut rtk = guard.as_mut().as_pin_mut().unwrap();
                if !rtk.as_mut().has_endpoint(rtk_ep_num) {
                    continue;
                }
                let ep_guard = self.endpoints[ep as usize].lock();
                ep_guard.stop(rtk.as_mut())?;
            }
            self.ep_shutdown.wait();
            self.ep_shutdown.reset();
        }
        Ok(())
    }

    fn patch_bootargs(
        &self,
        aop_mmio: &IoMem<AOP_MMIO_SIZE>,
        patches: &[(u32, u64)],
    ) -> Result<()> {
        let aop_mmio = aop_mmio.relaxed();
        let offset = aop_mmio.read32(BOOTARGS_OFFSET) as usize;
        let size = aop_mmio.read32(BOOTARGS_SIZE) as usize;
        let mut arg_bytes = KVec::<u8>::from_elem(0, size, GFP_KERNEL)?;
        aop_mmio.try_memcpy_fromio(&mut arg_bytes, offset)?;
        let mut idx = 0;
        while idx < size {
            let key = u32::from_le_bytes(arg_bytes[idx..idx + 4].try_into().unwrap());
            let size = u32::from_le_bytes(arg_bytes[idx + 4..idx + 8].try_into().unwrap()) as usize;
            idx += 8;
            for (k, v) in patches.iter() {
                if *k != key {
                    continue;
                }
                arg_bytes[idx..idx + size].copy_from_slice(&(*v as u64).to_le_bytes()[..size]);
                break;
            }
            idx += size;
        }
        aop_mmio.try_memcpy_toio(offset, &arg_bytes)
    }

    /// One setup-port message, in interrupt context (the native host's
    /// boot_step() and endpoint_request()).
    fn setup_handle(&self, st: &mut SetupState, ep: u8, word: u64) -> Result<()> {
        if ep == SETUP_MGMT_EP {
            let ty = (word >> 52) & 0xff;
            match ty {
                SETUP_TYPE_HELLO => {
                    if word & 0xffffffff != SETUP_HELLO_VERSION {
                        dev_warn!(
                            self.dev,
                            "setup port: HELLO version {:#x}",
                            word & 0xffffffff
                        );
                    }
                    st.send(
                        SETUP_MGMT_EP,
                        (SETUP_TYPE_HELLO_REPLY << 52) | SETUP_HELLO_VERSION,
                    )
                }
                SETUP_TYPE_EPMAP => {
                    // The firmware advertises its endpoint set here, and the
                    // driver otherwise discards it and requests SETUP_ENDPOINTS
                    // blindly.  RTKit packs a 32-bit bitmap plus a 3-bit base,
                    // so the endpoint number is base * 32 + bit.  Log it: the
                    // J700 map differs from the M1/M2 one this list was written
                    // for, and a missing endpoint stalls firmware startup.
                    let bitmap = (word & 0xffffffff) as u32;
                    let base = ((word >> 32) & 0x7) as u8;
                    for bit in 0..32u8 {
                        if bitmap & (1u32 << bit) != 0 {
                            dev_info!(
                                self.dev,
                                "setup port: firmware advertises endpoint {:#04x}\n",
                                base * 32 + bit
                            );
                        }
                    }
                    st.send(SETUP_MGMT_EP, word)?;
                    if word & SETUP_EPMAP_LAST != 0 {
                        st.map_done = true;
                    }
                    Ok(())
                }
                SETUP_TYPE_AP_PWR => {
                    st.ap_ready = (word & 0xffff) == SETUP_AP_PWR_ON;
                    Ok(())
                }
                SETUP_TYPE_UNK3 => st.send(SETUP_MGMT_EP, SETUP_TYPE_UNK3_REPLY << 52),
                SETUP_TYPE_PWR_ACK => Ok(()),
                _ => Err(EIO),
            }
        } else if word >> 56 == SETUP_BUFFER_REQUEST {
            self.setup_endpoint_request(st, ep, word)
        } else if st.endpoint(ep).is_some() {
            st.reply = Some(word);
            Ok(())
        } else {
            Err(EIO)
        }
    }

    /// The source-buffer bind: a 24-byte request in the endpoint's
    /// host-to-AOP page, (3 << 60) | len on the endpoint, the firmware's
    /// 0x2000000000000001, one status byte in the reply window, then
    /// 4 << 60 to release it.
    fn setup_bind_source(&self, iova: u64, size: u64) -> Result<()> {
        let mut st = self.setup.lock();
        let ep = st.endpoint(SETUP_SOURCE_EP).ok_or(ENXIO)?;
        let tx_off = st.arena_offset(ep.tx_iova).ok_or(EIO)?;
        let rx_off = st.arena_offset(ep.rx_iova).ok_or(EIO)?;
        let mut req = [0u8; 24];
        req[..8].copy_from_slice(&SETUP_SET_SOURCE_BUFFER.to_le_bytes());
        req[8..16].copy_from_slice(&iova.to_le_bytes());
        req[16..24].copy_from_slice(&size.to_le_bytes());
        {
            let arena = st.arena.as_mut().ok_or(ENXIO)?;
            // SAFETY: the arena is a coherent allocation of
            // SETUP_ARENA_PAGES pages and tx_off is a page inside it.
            unsafe {
                let buf = &mut arena.as_mut()[tx_off..tx_off + req.len()];
                buf.copy_from_slice(&req);
            }
        }
        mem_sync();
        st.reply = None;
        st.send(SETUP_SOURCE_EP, SETUP_REQUEST_TYPE | req.len() as u64)?;
        let mut waited = 0i64;
        while st.reply.is_none() {
            let _ = self
                .setup_cv
                .wait_interruptible_timeout(&mut st, msecs_to_jiffies(100));
            waited += 100;
            if waited >= SETUP_REPLY_TIMEOUT_MS && st.reply.is_none() {
                dev_err!(self.dev, "setup port: source bind timed out\n");
                return Err(ETIMEDOUT);
            }
        }
        let reply = st.reply.take().unwrap();
        if reply != SETUP_REPLY_READY {
            dev_err!(
                self.dev,
                "setup port: unexpected source bind reply {:#x}\n",
                reply
            );
            return Err(EIO);
        }
        mem_sync();
        let status = {
            let arena = st.arena.as_ref().ok_or(ENXIO)?;
            // SAFETY: rx_off is a page inside the arena.
            unsafe { arena.as_ref()[rx_off] }
        };
        st.send(SETUP_SOURCE_EP, SETUP_REQUEST_DONE)?;
        if status != 0 {
            dev_err!(self.dev, "setup port: source bind status {:#x}\n", status);
            return Err(EIO);
        }
        dev_info!(
            self.dev,
            "setup port: source ring {:#x} ({} bytes) bound\n",
            iova,
            size
        );
        Ok(())
    }

    /// Push the ALS calibration on setup-port endpoint 0x21.
    ///
    /// Without it the sensor answers every property correctly and accepts the
    /// arming interval, and then reports nothing at all, because the trusted
    /// side never powers the part.  With it the samples start immediately.
    /// Measured on a cold AOP under m1n1 with no secure kernel present, which
    /// is the point: the exclave is needed to learn these bytes, not to send
    /// them.
    fn setup_push_als_calibration(&self) -> Result<()> {
        let mut st = self.setup.lock();
        let ep = match st.endpoint(SETUP_ALS_EP) {
            Some(ep) => ep,
            None => {
                dev_warn!(
                    self.dev,
                    "setup port: no endpoint {:#04x}; ALS uncalibrated\n",
                    SETUP_ALS_EP
                );
                return Ok(());
            }
        };
        let tx_off = st.arena_offset(ep.tx_iova).ok_or(EIO)?;
        let rx_off = st.arena_offset(ep.rx_iova).ok_or(EIO)?;
        {
            let arena = st.arena.as_mut().ok_or(ENXIO)?;
            // SAFETY: the arena is a coherent allocation of SETUP_ARENA_PAGES
            // pages and tx_off is a page inside it.
            unsafe {
                let buf = &mut arena.as_mut()[tx_off..tx_off + SETUP_ALS_CALIBRATION.len()];
                buf.copy_from_slice(&SETUP_ALS_CALIBRATION);
            }
        }
        mem_sync();
        st.reply = None;
        st.send(
            SETUP_ALS_EP,
            SETUP_REQUEST_TYPE | SETUP_ALS_CALIBRATION.len() as u64,
        )?;
        let mut waited = 0i64;
        while st.reply.is_none() {
            let _ = self
                .setup_cv
                .wait_interruptible_timeout(&mut st, msecs_to_jiffies(100));
            waited += 100;
            if waited >= SETUP_REPLY_TIMEOUT_MS && st.reply.is_none() {
                dev_err!(self.dev, "setup port: ALS calibration timed out\n");
                return Err(ETIMEDOUT);
            }
        }
        let reply = st.reply.take().unwrap();
        if reply != SETUP_REPLY_READY && reply != SETUP_REPLY_READY_ALT {
            dev_err!(
                self.dev,
                "setup port: unexpected ALS calibration reply {:#x}\n",
                reply
            );
            return Err(EIO);
        }
        mem_sync();
        let status = {
            let arena = st.arena.as_ref().ok_or(ENXIO)?;
            // SAFETY: rx_off is a page inside the arena.
            unsafe { arena.as_ref()[rx_off] }
        };
        st.send(SETUP_ALS_EP, SETUP_REQUEST_DONE)?;
        dev_info!(
            self.dev,
            "setup port: ALS calibration pushed (reply {:#x}, status {:#x})\n",
            reply,
            status
        );
        Ok(())
    }

    /// Endpoint buffer request: one host-to-AOP page and a reply window of
    /// requested * 16 bytes, both handed out from the pre-mapped arena.
    fn setup_endpoint_request(&self, st: &mut SetupState, ep: u8, word: u64) -> Result<()> {
        let requested = (word & 0xffffffff) as usize;
        let slot = SETUP_ENDPOINTS.iter().position(|e| *e == ep).ok_or(EIO)?;
        if st.endpoints[slot].is_some() || (requested != 0x400 && requested != 0x1000) {
            return Err(EIO);
        }
        let rx_pages = (requested * 16).div_ceil(SETUP_PAGE);
        if st.next_page + 1 + rx_pages > SETUP_ARENA_PAGES {
            return Err(ENOMEM);
        }
        let base = st.arena_iova();
        let tx_iova = base + (st.next_page * SETUP_PAGE) as u64;
        st.next_page += 1;
        let rx_iova = base + (st.next_page * SETUP_PAGE) as u64;
        st.next_page += rx_pages;
        st.send(ep, SETUP_BUFFER_REQUEST_ACK)?;
        st.send(ep, SetupState::shared_descriptor(tx_iova, 1, true))?;
        st.send(ep, SetupState::shared_descriptor(rx_iova, rx_pages, false))?;
        st.endpoints[slot] = Some(SetupEndpoint {
            ep,
            tx_iova,
            rx_iova,
        });
        dev_info!(
            self.dev,
            "setup port: endpoint {:#x} tx {:#x} rx {:#x} ({} pages)\n",
            ep,
            tx_iova,
            rx_iova,
            rx_pages
        );
        Ok(())
    }

    /// After the primary port reports AP power ON: request the setup port's
    /// AP power state and wait for its ack and the five endpoint buffers.
    fn setup_finish_boot(&self) -> Result<()> {
        let mut st = self.setup.lock();
        if !st.map_done {
            dev_warn!(
                self.dev,
                "setup port: EPMAP not complete before AP power request\n"
            );
        }
        if !st.power_sent {
            st.send(SETUP_MGMT_EP, (SETUP_TYPE_AP_PWR << 52) | SETUP_AP_PWR_INIT)?;
            st.power_sent = true;
        }
        let mut waited = 0i64;
        while !st.ready() && !st.failed {
            let _ = self
                .setup_cv
                .wait_interruptible_timeout(&mut st, msecs_to_jiffies(100));
            waited += 100;
            if waited >= SETUP_BOOT_TIMEOUT_MS {
                break;
            }
        }
        let ready = st.ready();
        let count = st.endpoint_count();
        let ap_ready = st.ap_ready;
        let failed = st.failed;
        drop(st);
        if !ready {
            dev_err!(
                self.dev,
                "setup port: boot incomplete (ap_ready {} endpoints {}/{} failed {})\n",
                ap_ready,
                count,
                SETUP_ENDPOINTS.len(),
                failed
            );
            return Err(ETIMEDOUT);
        }
        dev_info!(self.dev, "setup port: dual-mailbox boot complete\n");
        self.dump_setup_rings();
        Ok(())
    }

    /// Endpoints 0x30 and 0x32 get setup-port tx/rx buffers and then nothing
    /// runs on them: they are not started as AFK/EPIC endpoints.  macOS reaches
    /// the SPU (and so the CT817 ambient light sensor) over a RemoteDataQueue
    /// pair on an iop_ringbuffer, not over EPIC, so one of these is the likely
    /// carrier.  An iop_ringbuffer is identifiable without any traffic: its
    /// header word at ring+4 carries the protocol version in the high half, and
    /// iop_ringbuffer_enqueue_msg_vec only emits the "AOP " magic when that is
    /// 0x0007xxxx.  Dump the head of every setup buffer so the ring -- if there
    /// is one -- names itself.
    fn dump_setup_rings(&self) {
        let st = self.setup.lock();
        let Some(arena) = st.arena.as_ref() else {
            return;
        };
        // SAFETY: the arena is a live coherent allocation of SETUP_ARENA_PAGES
        // pages and every offset below is bounds-checked against it.
        let bytes = unsafe { arena.as_ref() };
        for slot in st.endpoints.iter().flatten() {
            for (what, iova) in [("tx", slot.tx_iova), ("rx", slot.rx_iova)] {
                let Some(off) = st.arena_offset(iova) else {
                    continue;
                };
                if off + 64 > bytes.len() {
                    continue;
                }
                let head = &bytes[off..off + 64];
                let w0 = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
                let w1 = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
                let ring = (w1 & 0xffff0000) == 0x0007_0000;
                dev_info!(
                    self.dev,
                    "setup port: ep {:#x} {} head w0 {:#010x} w1 {:#010x}{} {:02x?}\n",
                    slot.ep,
                    what,
                    w0,
                    w1,
                    if ring { " IOP_RINGBUFFER(v7)" } else { "" },
                    head
                );
            }
        }
    }

    fn start_cpu(&self, asc_mmio: &RelaxedMmio<ASC_MMIO_SIZE>) -> Result<()> {
        let val = asc_mmio.read32(CPU_CONTROL);
        asc_mmio.write32(val | CPU_RUN, CPU_CONTROL);
        Ok(())
    }
}

impl AopData {
    /// Serializes the calls on an endpoint whose replies carry no tag; the
    /// guard is held until the reply (or the timeout) so that a reply can
    /// only belong to the one call in flight.
    fn take_call_turn(&self, ep_idx: u8) -> Option<MutexGuard<'_, ()>> {
        self.epic_v4.then(|| self.call_turn[ep_idx as usize].lock())
    }
    /// Waits for a call's reply with a bound; on timeout the call slot is
    /// released so a late reply is dropped instead of completing a later call.
    fn wait_call(
        &self,
        ep_idx: u8,
        svc: &EPICService,
        subtype: u16,
        call: Arc<FutureValue<CallResult>>,
    ) -> Result<CallResult> {
        match call.wait_timeout(EPIC_CALL_TIMEOUT_MS) {
            Some(res) => Ok(res),
            None => {
                let mut ep_guard = self.endpoints[ep_idx as usize].lock();
                ep_guard.cancel_call(&call);
                dev_err!(
                    self.dev,
                    "EPIC call {:#x} on channel {} timed out after {} ms\n",
                    subtype,
                    svc.channel,
                    EPIC_CALL_TIMEOUT_MS
                );
                Err(ETIMEDOUT)
            }
        }
    }
}

impl AOP for AopData {
    fn epic_call(&self, svc: &EPICService, subtype: u16, msg_bytes: &[u8]) -> Result<u32> {
        let ep_idx = afk_endpoint_index(svc.endpoint).ok_or(EINVAL)?;
        let call = {
            let mut rtk_guard = self.rtkit.lock();
            let mut rtk = rtk_guard.as_mut().as_pin_mut().unwrap();
            let mut ep_guard = self.endpoints[ep_idx].lock();
            ep_guard.epic_notify(self, rtk.as_mut(), svc.channel, subtype, msg_bytes, None)?
        };
        Ok(self.wait_call(ep_idx, svc, subtype, call)?.retcode)
    }
    fn epic_call_ret(
        &self,
        svc: &EPICService,
        subtype: u16,
        msg_bytes: &[u8],
        ret_len: usize,
    ) -> Result<(u32, KVec<u8>)> {
        let ep_idx = afk_endpoint_index(svc.endpoint).ok_or(EINVAL)?;
        let call = {
            let mut rtk_guard = self.rtkit.lock();
            let mut rtk = rtk_guard.as_mut().as_pin_mut().unwrap();
            let mut ep_guard = self.endpoints[ep_idx].lock();
            let mut ret_buf = KVec::new();
            ret_buf.resize(ret_len, 0, GFP_KERNEL)?;
            ep_guard.epic_notify(
                self,
                rtk.as_mut(),
                svc.channel,
                subtype,
                EPIC_CATEGORY_NOTIFY,
                msg_bytes,
                Some(ret_buf),
            )?
        };
        let res = self.wait_call(ep_idx, svc, subtype, call)?;
        Ok((res.retcode, res.extra_data.unwrap()))
    }

    fn add_fakehid_listener(
        &self,
        svc: EPICService,
        listener: Arc<dyn FakehidListener>,
    ) -> Result<()> {
        let mut guard = self.hid_listeners.lock();
        Ok(guard.push(ListenerEntry { svc, listener }, GFP_KERNEL)?)
    }
    fn remove_fakehid_listener(&self, svc: &EPICService) -> bool {
        let mut guard = self.hid_listeners.lock();
        for i in 0..guard.len() {
            if guard[i].svc == *svc {
                guard.swap_remove(i);
                return true;
            }
        }
        false
    }
    fn add_report_listener(
        &self,
        svc: EPICService,
        subtype: u16,
        listener: Arc<dyn ReportListener>,
    ) -> Result<()> {
        let mut guard = self.report_listeners.lock();
        Ok(guard.push(
            ReportListenerEntry {
                svc,
                subtype,
                listener,
            },
            GFP_KERNEL,
        )?)
    }
    fn remove_report_listener(&self, svc: &EPICService, subtype: u16) -> bool {
        let mut guard = self.report_listeners.lock();
        for i in 0..guard.len() {
            if guard[i].svc == *svc && guard[i].subtype == subtype {
                guard.swap_remove(i);
                return true;
            }
        }
        false
    }
    fn source_ring(&self, dev: &device::Device<Bound>, size: usize) -> Result<Arc<SourceRing>> {
        if !self.epic_v4 {
            return Err(ENODEV);
        }
        let mut guard = self.source_ring.lock();
        if let Some(ring) = guard.as_ref() {
            if ring.size() < size {
                dev_err!(
                    self.dev,
                    "source ring already bound with {} bytes\n",
                    ring.size()
                );
                return Err(EBUSY);
            }
            return Ok(ring.clone());
        }
        let buf = Coherent::<u8>::zeroed_slice(dev, size, GFP_KERNEL)?;
        let iova = buf.dma_handle() as u64;
        self.setup_bind_source(iova, size as u64)?;
        let ring = Arc::new(SourceRing { buf, iova }, GFP_KERNEL)?;
        *guard = Some(ring.clone());
        Ok(ring)
    }
    fn remove(&self) {
        if let Err(e) = self.stop() {
            dev_err!(self.dev, "Failed to stop AOP {:?}", e);
        }
        *self.rtkit.lock() = None;
        let guard = self.subdevices.lock();
        for pdev in &*guard {
            unsafe {
                bindings::platform_device_unregister(*pdev);
            }
        }
    }
}

struct NoBuffer;
impl rtkit::Buffer for NoBuffer {
    fn iova(&self) -> Result<usize> {
        unreachable!()
    }
    fn buf(&mut self) -> Result<IoSysMapRef<'_, u8>> {
        unreachable!()
    }
}

#[vtable]
impl rtkit::Operations for AopData {
    type Data = Arc<AopData>;
    type Buffer = NoBuffer;

    fn recv_message(data: <Self::Data as ForeignOwnable>::Borrowed<'_>, ep: u8, msg: u64) {
        let Some(index) = afk_endpoint_index(ep) else {
            dev_warn!(
                data.dev,
                "Message {:#x} on unexpected endpoint {:#04x}",
                msg,
                ep
            );
            return;
        };
        let mut guard = data.rtkit.lock();
        let mut rtk = guard.as_mut().as_pin_mut().unwrap();
        let mut ep_guard = data.endpoints[index].lock();
        let ret = ep_guard.recv_message(data, rtk.as_mut(), msg);
        if let Err(e) = ret {
            dev_err!(data.dev, "Failed to handle rtkit message, error: {:?}", e);
        }
    }

    fn crashed(data: <Self::Data as ForeignOwnable>::Borrowed<'_>, _crashlog: Option<&[u8]>) {
        dev_err!(data.dev, "AOP firmware crashed");
    }
}

#[repr(transparent)]
struct AopDriver(Arc<dyn AOP>);

struct AopHwConfig {
    ec0p: u64,
    alig: u64,
    aopt: u64,
    /// t8140: no bootarg patching, a second "setup" mailbox to boot through,
    /// EPIC v4 on the AFK endpoints.
    t8140: bool,
}

const HW_CFG_T8103: AopHwConfig = AopHwConfig {
    ec0p: 0x020000,
    aopt: 1,
    alig: 128,
    t8140: false,
};
const HW_CFG_T8112: AopHwConfig = AopHwConfig {
    ec0p: 0x020000,
    aopt: 0,
    alig: 128,
    t8140: false,
};
const HW_CFG_T6000: AopHwConfig = AopHwConfig {
    ec0p: 0x020000,
    aopt: 0,
    alig: 64,
    t8140: false,
};
const HW_CFG_T6020: AopHwConfig = AopHwConfig {
    ec0p: 0x0100_00000000,
    aopt: 0,
    alig: 64,
    t8140: false,
};
const HW_CFG_T8140: AopHwConfig = AopHwConfig {
    ec0p: 0,
    aopt: 0,
    alig: 0,
    t8140: true,
};

kernel::of_device_table!(
    OF_TABLE,
    MODULE_OF_TABLE,
    <AopDriver as platform::Driver>::IdInfo,
    [
        (of::DeviceId::new(c_str!("apple,t8103-aop")), &HW_CFG_T8103),
        (of::DeviceId::new(c_str!("apple,t8112-aop")), &HW_CFG_T8112),
        (of::DeviceId::new(c_str!("apple,t6000-aop")), &HW_CFG_T6000),
        (of::DeviceId::new(c_str!("apple,t6020-aop")), &HW_CFG_T6020),
        (of::DeviceId::new(c_str!("apple,t8140-aop")), &HW_CFG_T8140),
    ]
);

impl platform::Driver for AopDriver {
    type IdInfo = &'static AopHwConfig;

    const OF_ID_TABLE: Option<of::IdTable<Self::IdInfo>> = Some(&OF_TABLE);

    fn probe(
        pdev: &platform::Device<Core>,
        info: Option<&Self::IdInfo>,
    ) -> impl PinInit<Self, Error> {
        let cfg = info.ok_or(ENODEV)?;
        unsafe { pdev.dma_set_mask_and_coherent(DmaMask::new::<42>())? };
        let aop_req = pdev.io_request_by_index(0).ok_or(EINVAL)?;
        let aop_mmio = KBox::pin_init(aop_req.iomap_sized::<AOP_MMIO_SIZE>(), GFP_KERNEL)?;
        let asc_req = pdev.io_request_by_index(1).ok_or(EINVAL)?;
        let asc_mmio = KBox::pin_init(asc_req.iomap_sized::<ASC_MMIO_SIZE>(), GFP_KERNEL)?;
        let data = AopData::new(pdev, cfg.t8140)?;
        let aop_mmio = aop_mmio.access(pdev.as_ref())?;
        if !cfg.t8140 {
            data.patch_bootargs(
                aop_mmio,
                &[
                    (from_fourcc(b"EC0p"), cfg.ec0p),
                    (from_fourcc(b"nCal"), 0x0),
                    (from_fourcc(b"alig"), cfg.alig),
                    (from_fourcc(b"AOPt"), cfg.aopt),
                ],
            )?;
        } else {
        }
        let rtkit = rtkit::RtKit::<AopData>::new(pdev.as_ref(), None, 0, data.clone())?;
        *data.rtkit.lock() = Some(rtkit);
        if cfg.t8140 {
            // The setup port must be answered from the first HELLO on, so it
            // is opened (and its 13-page arena mapped through dart-aop stream
            // 1, the setup mailbox's own IOMMU stream) before the CPU runs.
            let setup_mbox = mailbox::Mailbox::<SetupPortCallback>::new_byname(
                pdev.as_ref(),
                c_str!("setup"),
                data.clone(),
            )?;
            let setup_dev = setup_mbox.device();
            // SAFETY: no DMA through the setup mailbox device is in flight yet.
            unsafe {
                to_result(bindings::dma_set_mask_and_coherent(
                    setup_dev.as_raw(),
                    DmaMask::new::<42>().value(),
                ))?;
            }
            // SAFETY: the mailbox provider is bound for as long as we hold the
            // mailbox client.
            let bound = unsafe { setup_dev.as_bound() };
            let arena =
                Coherent::<u8>::zeroed_slice(bound, SETUP_ARENA_PAGES * SETUP_PAGE, GFP_KERNEL)?;
            dev_info!(
                pdev.as_ref(),
                "setup port: arena {} pages at iova {:#x}\n",
                SETUP_ARENA_PAGES,
                arena.dma_handle() as u64
            );
            let mut st = data.setup.lock();
            st.arena = Some(arena);
            st.mbox = Some(setup_mbox);
            drop(st);
        }
        let asc_mmio = asc_mmio.access(pdev.as_ref())?.relaxed();
        let _ = data.start_cpu(asc_mmio);
        if cfg.t8140 {
            // Primary port RTKit v12 handshake to AP power ON, with the setup
            // port answered from interrupt context meanwhile; then the setup
            // port's own AP power state and endpoint buffers.
            {
                let mut guard = data.rtkit.lock();
                let mut rtk = guard.as_mut().as_pin_mut().unwrap();
                rtk.as_mut().wake()?;
            }
            data.setup_finish_boot()?;
            // Before any EPIC client opens the ALS: the trusted side wants the
            // calibration first, and a failure here must not cost us the AOP.
            if let Err(e) = data.setup_push_als_calibration() {
                dev_warn!(
                    pdev.as_ref(),
                    "t8140 AOP: ALS calibration failed ({:?})\n",
                    e
                );
            }
            dev_info!(
                pdev.as_ref(),
                "t8140 AOP: both ports up, starting AFK endpoints\n"
            );
            data.start_afk()?;
        } else {
            data.start()?;
        }
        let data = data as Arc<dyn AOP>;
        Ok(Self(data))
    }
}

impl Drop for AopDriver {
    fn drop(&mut self) {
        self.0.remove();
    }
}

unsafe impl Send for AopDriver {}

module_platform_driver! {
    type: AopDriver,
    name: "apple_aop",
    description: "AOP driver",
    license: "Dual MIT/GPL",
}

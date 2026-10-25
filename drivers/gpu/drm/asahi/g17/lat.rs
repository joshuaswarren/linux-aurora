// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Diagnostic only: an opt-in ring of host-side timestamps at the points a
//! command passes on its way through the driver (submit ioctl, firmware kick,
//! event message, event worker, fence signal), to break down submission round
//! trips. Off unless m5-lat/control is written; no hot-path printing.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use kernel::{debugfs, fmt, prelude::*, uaccess::UserSliceReader};

pub(crate) const SUBMIT_ENTER: u64 = 1;
pub(crate) const SUBMIT_EXIT: u64 = 2;
pub(crate) const KICK: u64 = 3;
pub(crate) const EVENT_MESSAGE: u64 = 4;
pub(crate) const EVENT_WORKER: u64 = 5;
pub(crate) const FENCE_SIGNAL: u64 = 6;
/// Compute retirement: the firmware's start and end ticks of the command (args).
pub(crate) const GPU_START: u64 = 7;
pub(crate) const GPU_END: u64 = 8;

const CAPACITY: usize = 65536;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static NEXT: AtomicUsize = AtomicUsize::new(0);
static DATA: [[AtomicU64; 2]; CAPACITY] = [const { [const { AtomicU64::new(0) }; 2] }; CAPACITY];

/// Record one point: the kind in the low byte, `arg` above it.
pub(crate) fn note(kind: u64, arg: u64) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    if index >= CAPACITY {
        return;
    }
    DATA[index][1].store(kind | (arg << 8), Ordering::Relaxed);
    DATA[index][0].store(super::now_ns(), Ordering::Release);
}

/// Write "on" (clears the ring), "off"; read for "ns kind arg" lines.
pub(crate) struct Control;
impl debugfs::Writer for Control {
    fn write(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = NEXT.load(Ordering::Acquire).min(CAPACITY);
        writeln!(f, "active={} records={}", ACTIVE.load(Ordering::Relaxed), count)?;
        for row in DATA.iter().take(count) {
            let ns = row[0].load(Ordering::Acquire);
            let word = row[1].load(Ordering::Relaxed);
            writeln!(f, "{} {} {}", ns, word & 0xff, word >> 8)?;
        }
        Ok(())
    }
}
impl debugfs::Reader for Control {
    fn read_from_slice(&self, reader: &mut UserSliceReader) -> Result {
        let mut bytes = [0u8; 8];
        let n = reader.len();
        if n == 0 || n > bytes.len() {
            return Err(EINVAL);
        }
        reader.read_slice(&mut bytes[..n])?;
        match core::str::from_utf8(&bytes[..n]).map_err(|_| EINVAL)?.trim() {
            "on" => {
                ACTIVE.store(false, Ordering::SeqCst);
                for row in DATA.iter() {
                    row[0].store(0, Ordering::Relaxed);
                }
                NEXT.store(0, Ordering::SeqCst);
                ACTIVE.store(true, Ordering::SeqCst);
            }
            "off" => ACTIVE.store(false, Ordering::SeqCst),
            _ => return Err(EINVAL),
        }
        Ok(())
    }
}

pub(crate) struct Files {
    control: Control,
}
pub(crate) type Handle = core::pin::Pin<KBox<debugfs::Scope<Files>>>;
pub(crate) fn open() -> Result<Handle> {
    KBox::pin_init(debugfs::Scope::dir(Files { control: Control }, c"m5-lat",
        |data, dir| {
            dir.read_write_file(c"control", &data.control);
        }), GFP_KERNEL)
}

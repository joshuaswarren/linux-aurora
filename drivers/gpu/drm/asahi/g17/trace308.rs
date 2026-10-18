//! Opt-in capture of firmware traces, events and callback entry. No hot-path printing.
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use kernel::{debugfs, fmt, prelude::*, uaccess::UserSliceReader};
use super::Role;

const CAPACITY: usize = 16384;
const WORDS: usize = 12;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static NEXT: AtomicUsize = AtomicUsize::new(0);
static DATA: [[AtomicU64; WORDS]; CAPACITY] =
    [const { [const { AtomicU64::new(0) }; WORDS] }; CAPACITY];

pub(super) fn capturing() -> bool { ACTIVE.load(Ordering::Relaxed) }

pub(super) fn record(role: Role, kind: u64, words: [u64; 9]) {
    if !ACTIVE.load(Ordering::Relaxed) { return; }
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    if index >= CAPACITY { return; }
    let row = &DATA[index];
    row[1].store(kind | ((role as u64) << 8), Ordering::Relaxed);
    row[2].store(super::now_ns(), Ordering::Relaxed);
    for (slot, value) in row[3..].iter().zip(words) {
        slot.store(value, Ordering::Relaxed);
    }
    row[0].store(index as u64 + 1, Ordering::Release);
}

pub(super) fn record_words(role: Role, kind: u64, words: [u32; 18]) {
    if !ACTIVE.load(Ordering::Relaxed) { return; }
    record(role, kind, core::array::from_fn(|i|
        u64::from(words[2 * i]) | (u64::from(words[2 * i + 1]) << 32)));
}

pub(crate) struct Control;
impl debugfs::Writer for Control {
    fn write(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "active={} reserved={} capacity={}",
                 ACTIVE.load(Ordering::Acquire), NEXT.load(Ordering::Acquire), CAPACITY)
    }
}
impl debugfs::Reader for Control {
    fn read_from_slice(&self, reader: &mut UserSliceReader) -> Result {
        let mut bytes = [0u8; 2];
        let n = reader.len();
        if n == 0 || n > 2 { return Err(EINVAL); }
        reader.read_slice(&mut bytes[..n])?;
        if n == 2 && bytes[1] != b'\n' { return Err(EINVAL); }
        match bytes[0] {
            b'0' => ACTIVE.store(false, Ordering::Release),
            b'1' if NEXT.load(Ordering::Acquire) == 0 => ACTIVE.store(true, Ordering::Release),
            b'1' => return Err(EBUSY),
            _ => return Err(EINVAL),
        }
        Ok(())
    }
}

pub(crate) struct Dump;
impl debugfs::Writer for Dump {
    fn write(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = NEXT.load(Ordering::Acquire);
        writeln!(f, "trace308 reserved={} capacity={} active={}",
                 count, CAPACITY, ACTIVE.load(Ordering::Acquire))?;
        for row in DATA.iter().take(count.min(CAPACITY)) {
            let commit = row[0].load(Ordering::Acquire);
            if commit == 0 { continue; }
            write!(f, "{}", commit)?;
            for value in &row[1..] { write!(f, " {}", value.load(Ordering::Relaxed))?; }
            writeln!(f)?;
        }
        Ok(())
    }
}
pub(crate) struct Capture { control: Control, dump: Dump }
pub(crate) type Handle = core::pin::Pin<KBox<debugfs::Scope<Capture>>>;
pub(crate) fn open() -> Result<Handle> {
    KBox::pin_init(debugfs::Scope::dir(
        Capture { control: Control, dump: Dump }, c"m5-trace308", |data, dir| {
            dir.read_write_file(c"control", &data.control);
            dir.read_only_file(c"records", &data.dump);
        }), GFP_KERNEL)
}

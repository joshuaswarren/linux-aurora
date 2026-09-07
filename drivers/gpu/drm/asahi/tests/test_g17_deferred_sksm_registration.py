#!/usr/bin/env python3
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
regs = (root / "regs.rs").read_text()
drm = (root / "g17_drm.rs").read_text()
manager = (root / "g17_manager.rs").read_text()


def block(source: str, marker: str) -> str:
    start = source.index(marker)
    brace = source.index("{", start)
    depth = 0
    for pos in range(brace, len(source)):
        if source[pos] == "{":
            depth += 1
        elif source[pos] == "}":
            depth -= 1
            if depth == 0:
                return source[start : pos + 1]
    raise AssertionError(f"unterminated block: {marker}")


configure = block(regs, "pub(crate) fn g17p_sksm_configure_queue(")
replay = block(regs, "pub(crate) fn g17p_replay_deferred_sksm_registrations(")
cancel = block(regs, "pub(crate) fn g17p_cancel_deferred_sksm_registration(")
wait = block(drm, "fn wait_render_slot(")
render_install = block(manager, "if !queue.registered {")
compute_install = block(manager, "pub(crate) fn register_prepared_compute_sksm_queue(")
compute_uninstall = block(manager, "pub(crate) fn unregister_compute_sksm_queue(")

assert "sksm_deferred_registrations" in regs
assert "stash_deferred_sksm_registration(pair, enable_pair)?" in configure
assert "stash_deferred_sksm_pair" not in configure
assert "return Err(ENOSPC)" in regs
assert replay.index("g17p_sksm_port_powered") < replay.index("g17p_sksm_publish_locked")
assert replay.index("registration.configure_pair") < replay.index("registration.enable_pair")
assert replay.index("registration.enable_pair") < replay.index("deferred.entries[index] = None")
assert "sksm_deferred.lock()" not in replay
assert "*slot = None" in cancel
assert wait.index("replay_deferred_render_sksm_registrations") < wait.index(
    "poll_deferred_render_completion"
)
assert render_install.index("g17p_sksm_configure_queue") < render_install.index(
    "queue.registered = true"
)
assert compute_install.index("g17p_sksm_configure_queue") < compute_install.index(
    "queue.hardware_state = G17PSksmQueueHardwareState::HardwareRegistered"
)
assert compute_uninstall.index("g17p_cancel_deferred_sksm_registration") < (
    compute_uninstall.index("g17p_sksm_publish_write_pair")
)

model = r'''
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Registration { configure: u8, enable: u8 }

#[derive(Debug, PartialEq, Eq)]
enum Error { Full, NotPowered, Write }

struct Pending {
    entries: [Option<Registration>; 2],
    generic: Vec<u8>,
}

impl Pending {
    fn new() -> Self { Self { entries: [None; 2], generic: Vec::new() } }

    fn stash(&mut self, registration: Registration) -> Result<(), Error> {
        if let Some(existing) = self.entries.iter_mut().find(|entry| {
            entry.is_some_and(|current| current.configure == registration.configure)
        }) {
            *existing = Some(registration);
            return Ok(());
        }
        let slot = self.entries.iter_mut().find(|slot| slot.is_none()).ok_or(Error::Full)?;
        *slot = Some(registration);
        Ok(())
    }

    fn cancel(&mut self, configure: u8) -> bool {
        let Some(slot) = self.entries.iter_mut().find(|entry| {
            entry.is_some_and(|registration| registration.configure == configure)
        }) else { return false };
        *slot = None;
        true
    }

    fn replay(&mut self, writer: &mut Writer) -> Result<usize, Error> {
        if !writer.powered { return Err(Error::NotPowered); }
        let mut issued = 0;
        for index in 0..self.entries.len() {
            let Some(registration) = self.entries[index] else { continue };
            writer.write(registration.configure)?;
            writer.write(registration.enable)?;
            self.entries[index] = None;
            issued += 1;
        }
        Ok(issued)
    }
}

struct Writer { powered: bool, fail_at: Option<usize>, writes: Vec<u8> }
impl Writer {
    fn write(&mut self, value: u8) -> Result<(), Error> {
        if self.fail_at == Some(self.writes.len()) { return Err(Error::Write); }
        self.writes.push(value);
        Ok(())
    }
}

#[test]
fn overflow_is_explicit_and_atomic() {
    let mut pending = Pending::new();
    pending.stash(Registration { configure: 1, enable: 2 }).unwrap();
    pending.stash(Registration { configure: 3, enable: 4 }).unwrap();
    assert_eq!(pending.stash(Registration { configure: 5, enable: 6 }), Err(Error::Full));
    assert_eq!(pending.entries, [
        Some(Registration { configure: 1, enable: 2 }),
        Some(Registration { configure: 3, enable: 4 }),
    ]);
}

#[test]
fn unpowered_replay_never_touches_the_port() {
    let mut pending = Pending::new();
    pending.stash(Registration { configure: 1, enable: 2 }).unwrap();
    let mut writer = Writer { powered: false, fail_at: None, writes: Vec::new() };
    assert_eq!(pending.replay(&mut writer), Err(Error::NotPowered));
    assert!(writer.writes.is_empty());
    assert!(pending.entries[0].is_some());
}

#[test]
fn replay_is_ordered_and_excludes_generic_traffic() {
    let mut pending = Pending::new();
    pending.generic.extend([90, 91]);
    pending.stash(Registration { configure: 1, enable: 2 }).unwrap();
    pending.stash(Registration { configure: 3, enable: 4 }).unwrap();
    let mut writer = Writer { powered: true, fail_at: None, writes: Vec::new() };
    assert_eq!(pending.replay(&mut writer), Ok(2));
    assert_eq!(writer.writes, [1, 2, 3, 4]);
    assert_eq!(pending.generic, [90, 91]);
    assert_eq!(pending.entries, [None, None]);
}

#[test]
fn failed_enable_keeps_the_complete_registration_pending() {
    let mut pending = Pending::new();
    pending.stash(Registration { configure: 1, enable: 2 }).unwrap();
    let mut failing = Writer { powered: true, fail_at: Some(1), writes: Vec::new() };
    assert_eq!(pending.replay(&mut failing), Err(Error::Write));
    assert_eq!(failing.writes, [1]);
    assert!(pending.entries[0].is_some());

    let mut retry = Writer { powered: true, fail_at: None, writes: Vec::new() };
    assert_eq!(pending.replay(&mut retry), Ok(1));
    assert_eq!(retry.writes, [1, 2]);
    assert!(pending.entries[0].is_none());
}

#[test]
fn replacement_is_bounded_and_cancelled_work_never_replays() {
    let mut pending = Pending::new();
    pending.stash(Registration { configure: 1, enable: 2 }).unwrap();
    pending.stash(Registration { configure: 1, enable: 7 }).unwrap();
    assert_eq!(pending.entries, [Some(Registration { configure: 1, enable: 7 }), None]);
    assert!(pending.cancel(1));
    assert!(!pending.cancel(1));
    let mut writer = Writer { powered: true, fail_at: None, writes: Vec::new() };
    assert_eq!(pending.replay(&mut writer), Ok(0));
    assert!(writer.writes.is_empty());
}
'''

with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / "test.rs"
    binary = Path(directory) / "test"
    source.write_text(model)
    subprocess.run(
        ["rustc", "--edition=2021", "--test", str(source), "-o", str(binary)],
        check=True,
    )
    subprocess.run([str(binary), "--test-threads=1"], check=True)

print(
    "PASS deferred SKSM registrations: explicit atomic overflow, powered-only replay, "
    "configure-before-enable ordering, generic traffic isolation, failure-safe retention, "
    "deduplication and cancellation"
)

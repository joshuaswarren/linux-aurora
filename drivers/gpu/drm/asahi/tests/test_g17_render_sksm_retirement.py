#!/usr/bin/env python3
"""Test the actual paired-completion occupancy transaction without hardware.

The producer types, retirement codec and manager transaction are verbatim.
Only the unrelated hardware-ID lease release is replaced with a fallible mock.
"""
from pathlib import Path
import re
import subprocess
import tempfile

src = Path(__file__).resolve().parent.parent
submission = (src / "g17_submission.rs").read_text()
manager = (src / "g17_manager.rs").read_text()


def block(text, pattern):
    match = re.search(pattern, text, re.M)
    assert match, pattern
    end = text.index("{", match.start()) + 1
    depth = 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[match.start():end]


poll = block(manager, r"^    pub\(crate\) fn poll_translated_render_completion\(")
complete = block(poll, r"^            if completion\.complete\s*")
start = complete.index("let retired_sksm =")
end = complete.index("self.render_hardware_buffer_id = None;")
transaction = complete[start:end]
# The real observer establishes queue retirement, timestamps and native list
# unlink before entering this transaction. Pending does not retire occupancy.
assert poll.index("observe_paired_render_window(") < poll.index("if completion.complete")
assert poll.count("complete_g17p_cl_kicks(") == complete.count("complete_g17p_cl_kicks(") == 2
assert transaction.index("queues.fragment.producer, 1)?") < transaction.index("queues.tiling.producer = ta;")
assert complete.index("queues.fragment.producer = fragment;") < complete.index("self.render_tracker = None;")

derive = "#[derive(Debug, Copy, Clone, PartialEq, Eq)]\n"
body = "\n".join([
    "#![allow(dead_code)]",
    "mod g17_submission {",
    *[derive + block(submission, r"^pub\(crate\) (?:struct|enum) " + n + r"\s+") for n in [
        "SubmissionError", "G17ChannelAbiTarget", "G17ClSubmissionBackend",
        "G17SksmQueueGeometry", "G17PClQosConfig", "G17PClKickProducerState"]],
    re.search(r"^pub\(crate\) const T8140_G17P_HAL200_SKSM_QUEUE_GEOMETRY: .*?;", submission, re.M | re.S).group(),
    block(submission, r"^pub\(crate\) fn complete_g17p_cl_kicks\("),
    "}",
    r'''
use g17_submission::*;
type Result<T = ()> = core::result::Result<T, i32>;
const EIO: i32 = 5;
const EOVERFLOW: i32 = 75;
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct Queue { producer: G17PClKickProducerState }
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct Queues { tiling: Queue, fragment: Queue }
struct Ids { fail: bool, releases: u32 }
impl Ids {
    fn release(&mut self, _id: u32) -> Result {
        if self.fail { return Err(EIO); }
        self.releases += 1;
        Ok(())
    }
}
struct Manager {
    render_sksm: Option<Queues>, render_submission_ordinal: u32,
    render_hardware_buffer_id: Option<u32>, render_hardware_buffer_ids: Ids,
}
impl Manager {
    fn actual_retirement_transaction(&mut self) -> Result {
''',
    transaction,
    "Ok(())\n}\n}",
    r'''
fn producer(qid: u8, submitted: u32) -> G17PClKickProducerState {
    G17PClKickProducerState { queue_id: qid,
        geometry: T8140_G17P_HAL200_SKSM_QUEUE_GEOMETRY, completion_selector: 2,
        qos: G17PClQosConfig { queue_byte_32: 0, class_40: 0x18, word_48: 0xffff },
        submitted, current_timestamp: 0x123456789a + qid as u64,
        last_add_kicks_timestamp: 0x1122334455, base_timestamp: 1,
        previous_valid: true, previous_timestamp: 0x2233445566 }
}
fn manager(ta: u32, fragment: u32) -> Manager {
    Manager { render_sksm: Some(Queues { tiling: Queue { producer: producer(0, ta) },
        fragment: Queue { producer: producer(1, fragment) } }),
        render_submission_ordinal: 999, render_hardware_buffer_id: Some(0),
        render_hardware_buffer_ids: Ids { fail: false, releases: 0 } }
}

#[test]
fn actual_pair_retires_one_each_without_resetting_any_history() {
    for (ta, fragment) in [(1, 1), (7, 13), (u32::MAX, u32::MAX)] {
        let mut m = manager(ta, fragment);
        let mut expected = m.render_sksm.unwrap();
        expected.tiling.producer.submitted -= 1;
        expected.fragment.producer.submitted -= 1;
        m.actual_retirement_transaction().unwrap();
        assert_eq!(m.render_sksm, Some(expected));
        assert_eq!(m.render_hardware_buffer_ids.releases, 1);
    }
}

#[test]
fn either_queue_underflow_aborts_before_both_mutations_and_id_release() {
    for (ta, fragment) in [(0, 1), (1, 0), (0, 0)] {
        let mut m = manager(ta, fragment);
        let before = m.render_sksm;
        assert_eq!(m.actual_retirement_transaction(), Err(EIO));
        assert_eq!(m.render_sksm, before);
        assert_eq!(m.render_hardware_buffer_ids.releases, 0);
    }
}

#[test]
fn failed_prerequisites_do_not_partially_retire_prepared_queue_states() {
    for failure in 0..3 {
        let mut m = manager(1, 1);
        match failure {
            0 => m.render_submission_ordinal = u32::MAX,
            1 => m.render_hardware_buffer_id = None,
            _ => m.render_hardware_buffer_ids.fail = true,
        }
        let before = m.render_sksm;
        assert_eq!(m.actual_retirement_transaction(), Err(if failure == 0 { EOVERFLOW } else { EIO }));
        assert_eq!(m.render_sksm, before);
        assert_eq!(m.render_hardware_buffer_ids.releases, 0);
    }
}

#[test]
fn a_completed_pair_cannot_decrement_again_without_new_published_work() {
    let mut m = manager(1, 1);
    m.actual_retirement_transaction().unwrap();
    let before = m.render_sksm;
    assert_eq!(m.actual_retirement_transaction(), Err(EIO));
    assert_eq!(m.render_sksm, before);
    assert_eq!(m.render_hardware_buffer_ids.releases, 1);
}
''',
])
with tempfile.TemporaryDirectory(prefix="g17-render-retirement-") as temp:
    binary = Path(temp) / "tests"
    subprocess.run(["rustc", "-Awarnings", "--edition=2021", "--test", "-", "-o", str(binary)],
                   input=body, text=True, check=True)
    subprocess.run([str(binary)], check=True)
print("Actual paired occupancy transaction, unchanged history, underflow and ordering PASS")

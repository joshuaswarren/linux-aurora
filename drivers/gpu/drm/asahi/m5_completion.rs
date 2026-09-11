// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Completion ownership for the bounded M5 TA/fragment pair.
//!
//! 25G76 GFX 0x17544 compares scheduler.completed with *scheduler.submitted;
//! 0x1763c unlinks the completed scheduler. The submitted total is host-owned,
//! not a firmware-decremented pending count. 0x17a68/0x17ae0 then write the
//! first stamp pointer and 0x17b98 publishes the corresponding event mask.

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub(crate) struct M5GraphicsCompletion {
    pub(crate) submitted: u32,
    pub(crate) completed: u32,
    pub(crate) linked: u32,
    pub(crate) list: [u64; 2],
    pub(crate) stamps: [u32; 2],
    pub(crate) signalled: u128,
}

impl M5GraphicsCompletion {
    /// This diagnostic owns qids 10/11 and event stamp 0x100 on a fresh boot.
    /// Queue retirement and framebuffer verification remain separate gates.
    pub(crate) fn pair_complete(self, list_address: u64) -> bool {
        list_address != 0
            && self.submitted == 2
            && self.completed == self.submitted
            && self.linked == 0
            && self.list == [0, list_address]
            && self.stamps == [0x100; 2]
            && self.signalled & 0xc00 == 0xc00
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: u64 = 0xffff_fc20_c002_0000;

    fn finished() -> M5GraphicsCompletion {
        M5GraphicsCompletion { submitted: 2, completed: 2, linked: 0,
            list: [0, LIST], stamps: [0x100; 2], signalled: 0xc00 }
    }

    #[test]
    fn submitted_total_does_not_decrement_on_completion() {
        assert!(finished().pair_complete(LIST));
        assert!(!M5GraphicsCompletion { submitted: 0, ..finished() }.pair_complete(LIST));
    }

    #[test]
    fn rendered_but_stranded_pair_is_not_complete() {
        // Retained Linux draw before sharing the two channels' job list.
        let stranded = M5GraphicsCompletion { completed: 2, linked: 1,
            list: [0xffff_fc20_c100_8000, LIST], stamps: [0; 2],
            signalled: 0, ..finished() };
        assert!(!stranded.pair_complete(LIST));
    }

    #[test]
    fn each_stage_needs_its_own_current_stamp_and_signal() {
        for stage in 0..2 {
            let mut state = finished();
            state.stamps[stage] = 0;
            assert!(!state.pair_complete(LIST));
            state.stamps[stage] = 0x200;
            assert!(!state.pair_complete(LIST));
            state = finished();
            state.signalled &= !(1 << (stage + 10));
            assert!(!state.pair_complete(LIST));
        }
    }

    #[test]
    fn empty_or_partial_observations_cannot_release_the_pair() {
        assert!(!M5GraphicsCompletion::default().pair_complete(LIST));
        assert!(!M5GraphicsCompletion { completed: 1, ..finished() }.pair_complete(LIST));
        assert!(!M5GraphicsCompletion { list: [0, LIST + 8], ..finished() }.pair_complete(LIST));
        assert!(!M5GraphicsCompletion { linked: 1, ..finished() }.pair_complete(LIST));
        assert!(!finished().pair_complete(0));
    }
}

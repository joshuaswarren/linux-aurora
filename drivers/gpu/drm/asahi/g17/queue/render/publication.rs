// SPDX-License-Identifier: GPL-2.0-only AND MIT

//! Preflighted item-ring publications. Each lane retains an independent cursor
//! and omits its configuration record only when its durable identity matches.

use super::{memory, retirement::Window};
use crate::g17::fw::{
    m5,
    kick::KickTimestamp,
    queue::{DataMaster, KickAnnounce, QueueConfig, ITEM_RING_ENTRIES},
};
use core::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use kernel::prelude::*;

fn identity(mut config: QueueConfig) -> QueueConfig {
    config.install = 0;
    config.context_update = 0;
    config.completion_seed_valid = 0;
    config.completion_seed = 0;
    config.qos_update = 0;
    config
}
pub(in crate::g17) fn required(previous: Option<QueueConfig>, config: QueueConfig) -> bool {
    config.install != 0
        || config.context_update != 0
        || config.completion_seed_valid != 0
        || config.qos_update != 0
        || previous.map(identity) != Some(identity(config))
}

enum Announcement {
    Neo(KickAnnounce),
    M5(m5::queue::Notification),
}

pub(in crate::g17) struct Lane<'a> {
    items: [&'a AtomicU64; 3],
    producer: &'a AtomicU32,
    values: [u64; 3],
    config: QueueConfig,
    config_destination: *mut u64,
    config_present: bool,
    announce: Announcement,
    announce_destination: *mut u8,
    prefix: usize,
    prefix_end: u32,
    pub(super) window: Window,
}
impl<'a> Lane<'a> {
    pub(in crate::g17) fn new(
        graph: &'a memory::Graph,
        stage: usize,
        ordinal: u64,
        base: u32,
        config: QueueConfig,
        present: bool,
        timestamp: KickTimestamp,
    ) -> Result<Self> {
        let announce = if graph.is_m5() {
            Announcement::M5(m5::queue::notification(config.qid.try_into()?, timestamp)?)
        } else { Announcement::Neo(KickAnnounce::new(
            config.qid.try_into()?,
            if stage == 0 { DataMaster::Tiling } else { DataMaster::Fragment },
            timestamp,
            config.priority as u8,
        )?) };
        Self::build(graph, stage, ordinal, base, config, present, announce)
    }

    fn build(
        graph: &'a memory::Graph,
        stage: usize,
        ordinal: u64,
        base: u32,
        config: QueueConfig,
        present: bool,
        announce: Announcement,
    ) -> Result<Self> {
        let m5 = matches!(announce, Announcement::M5(_));
        let count = u32::from(present) + if m5 { 1 } else { 2 };
        let window = Window::new(base, count, ITEM_RING_ENTRIES)?;
        let offset = ordinal as usize % memory::RECORDS;
        let config_offset = memory::CONFIG[stage] + offset * memory::CONFIG_STRIDE;
        let announce_offset = memory::ANNOUNCE[stage] + offset * memory::ANNOUNCE_STRIDE;
        let config_destination = graph
            .config
            .pointer(config_offset, size_of::<QueueConfig>())?
            .cast::<u64>();
        if config_destination.align_offset(align_of::<u64>()) != 0 {
            return Err(EINVAL);
        }
        let announce_destination = graph
            .queues
            .pointer(announce_offset, if m5 { size_of::<m5::queue::Notification>() }
                else { size_of::<KickAnnounce>() })?;
        let index =
            |delta: u32| memory::ITEMS[stage] + ((base + delta) % ITEM_RING_ENTRIES) as usize * 8;
        let items = [
            graph.queues.dword(index(0))?,
            graph.queues.dword(index(1))?,
            graph.queues.dword(index(2))?,
        ];
        let announce_va = graph.queues.gpu_va() + announce_offset as u64;
        let descriptor = graph.descriptor_va(stage, ordinal);
        let values = if m5 && present {
            [graph.config.gpu_va() + config_offset as u64, announce_va, 0]
        } else if m5 {
            [announce_va, 0, 0]
        } else if present {
            [
                descriptor,
                graph.config.gpu_va() + config_offset as u64,
                announce_va,
            ]
        } else {
            [descriptor, announce_va, 0]
        };
        Ok(Self {
            items,
            producer: graph.queues.word(memory::POINTERS[stage] + 0x40)?,
            values,
            config,
            config_destination,
            config_present: present,
            announce,
            announce_destination,
            prefix: count as usize - 1,
            prefix_end: (base + count - 1) % ITEM_RING_ENTRIES,
            window,
        })
    }
    pub(in crate::g17) fn target(&self) -> u32 { self.window.target() }
    fn items(&self, range: core::ops::Range<usize>) {
        for index in range {
            self.items[index].store(self.values[index], Ordering::Relaxed);
        }
    }
    fn config(&self) {
        if !self.config_present {
            return;
        }
        let source = (&self.config as *const QueueConfig).cast::<u64>();
        for index in 0..size_of::<QueueConfig>() / 8 {
            // SAFETY: QueueConfig is packed integer storage with no padding;
            // every source qword is read unaligned. The destination is a
            // checked, aligned record owned by this unpublished ordinal.
            unsafe {
                self.config_destination
                    .add(index)
                    .write(source.add(index).read_unaligned());
            }
        }
    }
    fn announce(&self) {
        // SAFETY: The full record is inside the retained queue graph and is
        // exclusively owned by this ordinal. Publication follows its copy.
        unsafe {
            match &self.announce {
                Announcement::Neo(value) => self.announce_destination
                    .cast::<KickAnnounce>().write_unaligned(*value),
                Announcement::M5(value) => self.announce_destination
                    .cast::<m5::queue::Notification>().write_unaligned(*value),
            }
        }
    }
    /// Fill the retained records without advancing the firmware-visible cursor.
    /// M5's host-port transaction must occur between this and publish_kick.
    pub(in crate::g17) fn prepare(&self) {
        self.items(0..self.prefix + 1);
        self.config();
        self.announce();
    }
    pub(super) fn publish_prefix(&self) {
        self.items(0..self.prefix);
        self.config();
        fence(Ordering::SeqCst);
        self.producer.store(self.prefix_end, Ordering::Relaxed);
    }
    pub(in crate::g17) fn publish_kick(&self) {
        self.announce();
        self.items(self.prefix..self.prefix + 1);
        fence(Ordering::SeqCst);
        self.producer.store(self.window.target(), Ordering::Relaxed);
    }
    pub(super) fn publish_tiling(
        &self,
        event_slot: &AtomicU32,
        next_slot: u32,
        inner: &AtomicU32,
        next_inner: u32,
    ) {
        self.items(0..self.prefix + 1);
        self.config();
        self.announce();
        fence(Ordering::SeqCst);
        self.producer.store(self.window.target(), Ordering::Relaxed);
        event_slot.store(next_slot, Ordering::Relaxed);
        inner.store(next_inner, Ordering::Relaxed);
    }
}

// SPDX-License-Identifier: GPL-2.0-only
//! Explicit arithmetic qualification through shared GPUVM and kick ownership.
//! This probe retains its installed graph until reboot; it is not DRM submission.
use super::{fw, kick, qos, context::{Context, WorkStateLease}, object::{Allocator, CpuMap, KernelObject, Placement}, Firmware};
use super::fw::{m5::compute, queue::*};
use super::m5_test_program as program;
use super::queue::compute::retirement::{self, Batch, Cursors, Recordless, Ticket};
use crate::{driver::AsahiDevice, mmu};
use core::sync::atomic::{fence, Ordering};
use kernel::{prelude::*, sync::Arc, time::{delay::fsleep, Delta}};
const PAGE: usize = mmu::UAT_PGSZ;
const POLICY: Policy = Policy::new(2, 2, 4, 8192);
const POINTERS: usize = 0x100;
const ITEMS: usize = 0x4000;
const CONFIG: usize = 0x8000;
const ANNOUNCE: usize = 0x8100;
const USAGE: usize = 0x8000;
const STAMPS: usize = 0xc000;

pub(super) struct Compute {
    aliases: KVec<mmu::KernelMapping>,
    context: Arc<Context>,
    current_work: Option<Arc<WorkStateLease>>,
    graph: KernelObject,
    descriptor: KernelObject,
    kick: kick::Queue,
    objects: KVec<KernelObject>,
    descriptor_low: u64,
    operand: u64,
    buffers: usize,
    pool: super::freelist::ComputePool,
    id: kick::Id,
    observations: Batch,
}

impl Compute {
    pub(super) fn new(dev: &AsahiDevice, firmware: &mut Firmware) -> Result<KBox<Self>> {
        let uat = &firmware.uat;
        let alloc = Allocator { dev, uat };
        let vm = uat.new_vm(1, uat.geometry().user_range())?.with_status()?;
        let execution = uat.new_execution_context(&vm)?;
        let context = Arc::new(Context::new(&alloc, &firmware.pool,
            &firmware.queues.qos_ids, execution, 0, POLICY)?, GFP_KERNEL)?;
        let owner = firmware.queues.reserve_channel_ids(1)?.start;
        let id = firmware.queues.qids.reserve(owner, DataMaster::Compute, Some(11))?;
        let mut kick = kick::Queue::compute(&alloc, id)?;
        let mut aliases = KVec::new();
        aliases.push(kick.map_client(&vm)?, GFP_KERNEL)?;
        let prot = mmu::PROT_GPU_FW_SHARED_RW;
        let cpu = CpuMap::WriteCombined;
        let mut graph = alloc.kernel(0x10000, PAGE as u64, prot, cpu)?;
        super::queue::compute::initialize_graph(&mut graph, &context, true)?;
        let descriptor = alloc.kernel(0x10000, PAGE as u64, prot, cpu)?;
        let low = descriptor.alias_in(uat.kernel_lower_vm(), crate::hw::t8140::dynamic::LOWER,
            PAGE as u64, mmu::PROT_GPU_SHARED_RW)?;
        let descriptor_low = low.iova();
        aliases.push(low, GFP_KERNEL)?;
        aliases.push(descriptor.map_alias(&vm, descriptor_low, mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
        let mut objects = KVec::new();
        let mut buffers = 0;
        for (address, kind) in [
            (program::SHADER_PAGE, program::PageKind::Shader),
            (program::PIPELINE_PAGE, program::PageKind::Pipeline),
            (program::STREAM_PAGE, program::PageKind::Stream),
            (program::PREEMPT_PAGE, program::PageKind::Preempt),
            (program::BUFFER_PAGE, program::PageKind::Buffers),
            (program::ROBUSTNESS_PAGE, program::PageKind::Robustness),
        ] {
            let access = if address == program::SHADER_PAGE || address == program::STREAM_PAGE {
                mmu::PROT_GPU_SHARED_RO
            } else { mmu::PROT_GPU_SHARED_RW };
            let mut object = KernelObject::new(dev, &vm, Placement::At(address), PAGE, access, cpu)?;
            program::initialize_page(kind, object.bytes_mut()).map_err(|_| EINVAL)?;
            if address == program::BUFFER_PAGE { buffers = objects.len(); }
            objects.push(object, GFP_KERNEL)?;
        }
        let mut pool = super::freelist::ComputePool::new(&alloc, id.qid(), true, None)?;
        aliases.push(pool.map_client(&vm)?, GFP_KERNEL)?;
        firmware.init.install_compute_pool(pool.descriptor()?)?;
        let operand = alloc.lower(compute::OPERAND_BYTES, PAGE as u64,
            mmu::PROT_GPU_SHARED_RW, cpu)?;
        let operand_va = operand.gpu_va();
        aliases.push(operand.map_alias(&vm, operand_va, mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
        objects.push(operand, GFP_KERNEL)?;
        let high = descriptor.gpu_va();
        dev_info!(dev.as_ref(), "M5 compute graph: context {}, queue {}, descriptor {:#x}/{:#x}\n",
            context.id(), id.qid(), high, descriptor_low);
        Ok(KBox::new(Self { aliases, context, current_work: None, graph, descriptor, kick, objects,
            descriptor_low, operand: operand_va, buffers, pool, id,
            observations: Batch::new()? }, GFP_KERNEL)?)
    }

    fn config(&self, install: bool) -> Result<QueueConfig> {
        QueueConfig::new(&QueueConfigArgs {
            target: QueueConfigTarget::Compute,
            kick_ring_va: self.kick.low_va(), kick_ring_fw_va: self.kick.firmware_va(),
            qid: self.id.qid(), install, completion_seed: None,
            context_id: self.context.id().try_into()?,
            free_list: FreeListBinding { va: if install {0} else {self.pool.control_va()},
                generation: if install {u64::MAX} else {1}, slot: if install {u32::MAX} else {u32::from(self.pool.id())} },
            scheduler_va: self.context.scheduler_va(), policy: POLICY,
            qos_slot: self.context.qos_id(), qos_update: install, owner_pid: 0, context_update: !install,
        })
    }

    fn publish(&self, firmware: &mut Firmware, producer: u32, first: bool) -> Result {
        fence(Ordering::SeqCst);
        self.graph.word(POINTERS + 0x40)?.store(producer, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        let slot = fw::channels::WorkSlot::new(DataMaster::Compute,
            self.graph.gpu_va(), self.id.qid(), producer.try_into()?, first)?;
        firmware.init.publish_work(POLICY.priority(), &slot)?;
        firmware.primary.notify(0x0083_0000_0000_0002 | (u64::from(POLICY.priority()) << 2))
    }

    fn wait_inner(&self, firmware: &Firmware, target: u32) -> Result {
        for _ in 0..2000 {
            if firmware.primary.state.crashed.load(Ordering::Acquire) { return Err(EIO); }
            if self.graph.word(POINTERS)?.load(Ordering::Acquire) == target { return Ok(()); }
            fsleep(Delta::from_millis(1));
        }
        Err(ETIMEDOUT)
    }

    pub(super) fn run(&mut self, dev: &AsahiDevice, firmware: &mut Firmware) -> Result {
        self.graph.write(CONFIG, self.config(true)?)?;
        self.graph.write(ITEMS, self.graph.gpu_va() + CONFIG as u64)?;
        firmware.queues.qids.publish(self.id)?;
        self.publish(firmware, 1, true)?;
        self.wait_inner(firmware, 1)?;
        dev_info!(dev.as_ref(), "M5 compute queue installed\n");
        if let Err(error) = firmware.acquire_registration_power() {
            dev_info!(dev.as_ref(), "M5 compute power acquire failed: {:?}, powered {}, counters {:?}\n",
                error, firmware.bridge.regs.g17_configuration_powered(),
                firmware.init.control_counters());
            firmware.init.dump_m5_failure(dev)?;
            return Err(error);
        }
        for generation in 1..=512u32 {
            self.dispatch(dev, firmware, generation)?;
            for role in [super::Role::Primary, super::Role::Secondary] {
                let mut events = super::channel::Events::new(role);
                while let Some(event) = events.next(dev.as_ref(), &firmware.init)? {
                    if matches!(event, fw::channels::Event::Restart { .. }) { return Err(EIO); }
                }
                firmware.rings.drain_trace(dev.as_ref(), &firmware.init, role);
                firmware.rings.drain_statistics(dev.as_ref(), &firmware.init, role);
            }
        }
        firmware.release_registration_power()?;
        dev_info!(dev.as_ref(), "M5 compute repeat PASS: 512 dispatches, 524288 exact outputs, kick ring wrapped, shared retirement, work-state and private-pool counters passed\n");
        Ok(())
    }

    fn dispatch(&mut self, dev: &AsahiDevice, firmware: &mut Firmware, generation: u32) -> Result {
        let high = self.descriptor.gpu_va();
        let timestamp = self.kick.timestamp();
        let stamp_target = generation.checked_mul(0x100).ok_or(EOVERFLOW)?;
        let producer = self.graph.word(POINTERS + 0x40)?.load(Ordering::Acquire);
        if self.graph.word(POINTERS)?.load(Ordering::Acquire) != producer
            || self.graph.word(POINTERS + 0x30)?.load(Ordering::Acquire) != producer {
            return Err(EBUSY);
        }
        for index in 0..program::ELEMENTS {
            self.objects[self.buffers].word(program::OUTPUT_OFFSET + index * 4)?
                .store(program::SENTINEL, Ordering::Relaxed);
        }
        if self.current_work.is_some() { return Err(EBUSY); }
        let work = Arc::new(WorkStateLease::deferred(self.context.clone()), GFP_KERNEL)?;
        let work_word = work.acquire()?;
        let work_node = work.node_va()?;
        work.add_submitted_kicks(1)?;
        self.current_work = Some(work.clone());
        let pool_target = self.pool.reserve_submission()?;
        if pool_target != u64::from(generation) { return Err(EIO); }
        let args = compute::DescriptorArgs {
            registers: compute::RegisterArgs {
                preempt_va: program::PREEMPT_PAGE, cdm_va: program::STREAM_PAGE,
                operand_va: self.operand, usage_va: program::ROBUSTNESS_PAGE,
                free_list_slot: u32::from(self.pool.id()), context_id: self.context.id().try_into()?, work_state: work_word,
                object_ids: 0x0000_0001_0000_0001, resource_policy: 0xf5_0040_0020,
                preempt_state_offset: 0x1480, operand_region_bytes: 0xf000,
                operand_aux_offset: 0x1f000,
                resource_control: 0x54b, dispatch_control: 0x10c08860,
                auxiliary_control: 0,
            },
            descriptor_va: self.descriptor_low, submit_sequence: 0,
            work_state_va: work_node, cdm_end_va: program::STREAM_PAGE + 44,
            stamp_va: [high + STAMPS as u64, high + STAMPS as u64 + 0x40], stamp_value: stamp_target,
            qid: u32::from(self.id.qid()), kick_timestamp: timestamp.get(),
            completion_ordinal: 0,
            status_va: [high + STAMPS as u64 + 0x80, high + STAMPS as u64 + 0x88],
            timestamp_va: [0;2], shared_control_va: self.pool.control_va(),
            event_metadata: [0;16], usage_fw_va: high + USAGE as u64, completion_identity: 1,
        };
        self.descriptor.initialize::<compute::Descriptor>(0, |d| d.write(&args))?;
        if generation == 1 {
            let mut config=self.config(false)?;
            config.completion_seed_valid=1;
            config.completion_seed=0;
            self.graph.write(CONFIG + 0x200,config)?;
        }
        self.graph.write(ANNOUNCE,fw::m5::queue::notification(self.id.qid(),timestamp)?)?;
        let kick_args = fw::kick::KickArgs {
            qid: self.id.qid(), timestamp, parent: self.kick.parent(), barriers: &[],
            dependency_types: Some(&[2]), descriptor_va: high, queue_va: self.graph.gpu_va(),
            qos: fw::kick::KickQos {slot: self.context.qos_id(), class: 8}, mcache: None,
            event_mask: [0,0,8,0], register_arrays: compute::bindings(self.descriptor_low)?,
            compute_scratch: false, priority: POLICY.priority(),
        };
        let prepared = self.kick.prepare_entry(&kick_args)?;
        prepared.prepare_install()?;
        prepared.commit();
        let mut target=producer;
        if generation==1 {
            self.graph.write(ITEMS+target as usize*8,self.graph.gpu_va()+CONFIG as u64+0x200)?;
            target=(target+1)%ITEM_RING_ENTRIES;
        }
        self.graph.write(ITEMS+target as usize*8,self.graph.gpu_va()+ANNOUNCE as u64)?;
        target=(target+1)%ITEM_RING_ENTRIES;
        let submission=fw::m5::queue::HardwareSubmission::compute(
            self.id.qid(),self.kick.low_va(),POLICY.priority(),generation==1)?;
        let owner=qos::Owner{qid:self.id.qid(),qos:self.context.qos_id(),data_master:DataMaster::Compute as u8};
        firmware.queues.accounting.publish(&qos::View::new(firmware.init.qos()?)?,owner,
            self.context.scheduler_va(),POLICY,qos::clock)?;
        self.context.mark_published();
        fence(Ordering::SeqCst);
        firmware.submit_m5_work(&submission)?;
        self.publish(firmware, target, false)?;
        let mut waited = self.wait_inner(firmware, target);
        if waited.is_ok() {
            waited = Err(ETIMEDOUT);
            for _ in 0..2000 {
                if firmware.primary.state.crashed.load(Ordering::Acquire) { break; }
                if self.descriptor.word(STAMPS)?.load(Ordering::Acquire) == stamp_target
                    && work.completed_kicks()? == 1
                    && self.pool.completed_submissions()? == pool_target {
                    waited = Ok(());
                    break;
                }
                fsleep(Delta::from_millis(1));
            }
        }
        let stamp = self.descriptor.word(STAMPS)?.load(Ordering::Acquire);
        let mut wrong = 0; let mut changed = 0; let mut untouched = 0;
        // Device memory reads follow all observed completion writes.
        unsafe { core::arch::asm!("dmb oshld", options(nostack, preserves_flags)); }
        for index in 0..program::ELEMENTS {
            let input = self.objects[self.buffers].word(program::INPUT_OFFSET + index * 4)?.load(Ordering::Relaxed);
            let output = self.objects[self.buffers].word(program::OUTPUT_OFFSET + index * 4)?.load(Ordering::Relaxed);
            changed += usize::from(input != index as u32 * 7);
            wrong += usize::from(output != index as u32 * 21 + 5);
            untouched += usize::from(output == program::SENTINEL);
        }
        if waited.is_err() || wrong != 0 || changed != 0 || generation % 64 == 0 {
        dev_info!(dev.as_ref(), "M5 compute result: wait {:?}, stamp {:#x}, wrong {}, untouched {}, changed inputs {}, inner [{}, {}, {}]\n",
            waited, stamp, wrong, untouched, changed,
            self.graph.word(POINTERS)?.load(Ordering::Relaxed),
            self.graph.word(POINTERS+0x30)?.load(Ordering::Relaxed),
            self.graph.word(POINTERS+0x40)?.load(Ordering::Relaxed));
        }
        if waited.is_err() {
            dev_info!(dev.as_ref(), "M5 compute raw fault: {:?}, queue configuration: {:x?}\n",
                firmware.bridge.regs.m5_fault_registers(),
                firmware.bridge.regs.m5_queue_configurations([self.id.qid(); 2]));
            for role in [super::Role::Primary, super::Role::Secondary] {
                let mut events = super::channel::Events::new(role);
                while let Some(event) = events.next(dev.as_ref(), &firmware.init)? {
                    dev_info!(dev.as_ref(), "M5 compute {:?} event {:?}\n", role, event);
                }
                firmware.rings.drain_trace(dev.as_ref(), &firmware.init, role);
                firmware.rings.drain_statistics(dev.as_ref(), &firmware.init, role);
            }
            firmware.init.dump_m5_failure(dev)?;
            dev_info!(dev.as_ref(), "M5 shared work state {:#x}, completed {}\n",
                work_word, work.completed_kicks()?);
            dev_info!(dev.as_ref(), "M5 pool completed {}/{}\n", self.pool.completed_submissions()?, pool_target);
            for (label, offset, count) in [("completion", STAMPS, 0x90 / 4)] {
                for i in 0..count {
                    let value = self.descriptor.word(offset + i * 4)?.load(Ordering::Relaxed);
                    if value != 0 { dev_info!(dev.as_ref(), "M5 {} +{:#x}={:#x}\n", label, i * 4, value); }
                }
            }
        }
        waited?;
        if stamp != stamp_target || wrong != 0 || changed != 0 { return Err(EIO); }
        // Qualify the production queue's completion decoder and retirement
        // predicate with the same publication, before reusing any descriptor.
        let ticket = Ticket {
            owner: 1, submission: u64::from(generation),
            context_id: self.context.id(), qid: self.id.qid(),
            descriptor: high, work_node,
            kick: timestamp.get(), ordinal: generation - 1,
            item_producer: target, item_count: ITEM_RING_ENTRIES, epoch: 0,
        };
        self.observations.clear();
        self.observations.request(&ticket)?;
        firmware.init.scan_compute_completions(|record| self.observations.fold(record))?;
        let observed = self.observations.take(&ticket)?.ok_or(ENODATA)?;
        let words = self.descriptor.read_words::<4>(0xf2c)?;
        let descriptor_times = [u64::from(words[0]) | (u64::from(words[1]) << 32),
            u64::from(words[2]) | (u64::from(words[3]) << 32)];
        if observed.timestamps[1] != descriptor_times[1]
            || observed.timestamps[0] > descriptor_times[0] {
            return Err(EIO);
        }
        let cursors = Cursors {
            producer: self.graph.word(POINTERS + 0x40)?.load(Ordering::Acquire),
            consumer: self.graph.word(POINTERS)?.load(Ordering::Acquire),
            count: ITEM_RING_ENTRIES,
        };
        if retirement::poll(&ticket, Some(observed), cursors, stamp,
            Recordless::NotYet, 0, None, false)? != Some(observed.timestamps) {
            return Err(EIO);
        }
        if generation == 1 {
            dev_info!(dev.as_ref(), "M5 shared compute retirement: record {:?}, descriptor {:?}\n",
                observed.timestamps, descriptor_times);
        }
        firmware.queues.accounting.complete(&qos::View::new(firmware.init.qos()?)?,&[owner])?;
        self.current_work.take();

        Ok(())
    }
}

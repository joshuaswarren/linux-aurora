// SPDX-License-Identifier: GPL-2.0-only
//! Explicit paired-queue qualification. Installed owners remain until reboot.
use super::{fw, kick, qos, context::{Context, WorkStateLease}, object::{Allocator, KernelObject}, Firmware};
use super::fw::queue::*;
use crate::{driver::AsahiDevice, mmu};
use core::sync::atomic::{fence, Ordering};
use kernel::{prelude::*, sync::Arc, time::{delay::fsleep, Delta}};
const POLICY: Policy = Policy::new(2, 2, 4, 8192);
use super::queue::render::{memory::{POINTERS, ITEMS, CONFIG, CONFIG_STRIDE, ANNOUNCE, ANNOUNCE_STRIDE}, publication::{self, Lane}};

pub(super) struct Pair {
    aliases: KVec<mmu::KernelMapping>,
    context: Arc<Context>,
    current_work: Option<Arc<WorkStateLease>>,
    kicks: [kick::Queue;2],
    ids: [kick::Id;2],
    resources: KBox<super::m5_render_resources::Resources>,
    pb_id: (u8,u64),
    input: Option<super::m5_render_input::Input>,
    descriptor_slots_seen: u32,
    workspace: KBox<fw::m5::render_registers::Workspace>,
    draws: u32,
    configs: [Option<QueueConfig>; 2],
    config_records: u32,
}

// Keep large temporary owners out of the firmware boot caller's stack frame.
// This runs once during explicit qualification, never on the submission path.
#[inline(never)]
fn heap_owner<T>(make: impl FnOnce() -> Result<T>) -> Result<KBox<T>> {
    Ok(KBox::new(make()?, GFP_KERNEL)?)
}

impl Pair {
    pub(super) fn new(dev: &AsahiDevice, firmware: &mut Firmware, clusters:u32) -> Result<KBox<Self>> {
        let pb_id=firmware.queues.buffers.allocate_render()?;
        let uat=&firmware.uat;
        let alloc=Allocator { dev, uat };
        let vm=uat.new_vm(2,uat.geometry().user_range())?.with_status()?;
        let execution=uat.new_execution_context(&vm)?;
        let context=Arc::new(Context::new(&alloc,&firmware.pool,
            &firmware.queues.qos_ids,execution,0,POLICY)?,GFP_KERNEL)?;
        let input=if matches!(*crate::module_parameters::m5_render_test.value(), 2 | 3) {
            Some(super::m5_render_input::Input::load(dev,&vm,uat.geometry().user_range())?)
        } else {None};
        let pass=input.as_ref().map_or_else(super::m5_render_resources::qualification_pass,|i|i.pass);
        let metrics=firmware._pm_metrics.as_ref().ok_or(EIO)?;
        let metrics_client=context.metrics_client(metrics)?;
        let metrics_lease=super::buffer::MetricsLease::new(&firmware.queues.metrics,metrics,
            fw::buffer::MetricsAddresses{client:metrics_client,firmware:firmware.init.metrics_fw_va()?})?;
        let channels=firmware.queues.reserve_channel_ids(2)?;
        let ta=firmware.queues.qids.reserve(channels.start,DataMaster::Tiling,Some(12))?;
        // QueueIds keys each engine independently, as in the shared render runtime.
        let frg=match firmware.queues.qids.reserve(channels.start+1,DataMaster::Fragment,Some(13)) {
            Ok(id)=>id,
            Err(error)=>{firmware.queues.qids.cancel_unpublished(ta)?;return Err(error);}
        };
        let ids=[ta,frg];
        let (ta,ta_alias)=kick::Queue::render(&alloc,ta,&vm)?;
        let (frg,frg_alias)=kick::Queue::render(&alloc,frg,&vm)?;
        let mut aliases=KVec::new();
        aliases.push(ta_alias,GFP_KERNEL)?;aliases.push(frg_alias,GFP_KERNEL)?;
        let private_pool = if *crate::module_parameters::m5_render_test.value() == 3 {
            super::m5_render_resources::qualify_pool_growth(&alloc, &context,
                &firmware.render_ids, &firmware._render_global,
                firmware.primary.state.shared.render_policy.as_ref().ok_or(EINVAL)?)?
        } else {
            Arc::new(super::freelist::RenderPool::m5(&alloc,
                super::freelist::RenderId::new(&firmware.render_ids)?, context.vm(),
                fw::m5::pool::PoolGeometry::g17s(7).map_err(|_| EINVAL)?)?, GFP_KERNEL)?
        };
        let mut resources=heap_owner(||super::m5_render_resources::Resources::new(
            &alloc,&firmware.pool,context.clone(),[ids[0].qid(),ids[1].qid()],
            &pass,clusters,pb_id.0,private_pool,metrics_lease))?;
        resources.view.context_id=context.id().try_into()?;
        resources.view.context_generation=context.render_generation();
        dev_info!(dev.as_ref(),"M5 render target and pool backing ready\n");
        let mut workspace=KBox::<fw::m5::render_registers::Workspace>::new_uninit(GFP_KERNEL)?;
        // SAFETY: Workspace contains only integer storage and implements Zeroable.
        unsafe { core::ptr::write_bytes(workspace.as_mut_ptr(),0,1); }
        // SAFETY: The entire allocation now has its valid all-zero representation.
        let workspace=unsafe { workspace.assume_init() };
        let mut pair=KBox::new(Self {
            aliases,context,current_work:None,kicks:[ta,frg],ids,resources,
            pb_id,input,descriptor_slots_seen:0,workspace,draws:0,configs:[None;2],config_records:0,
        },GFP_KERNEL)?;
        for stage in 0..2 {
            let config=fw::m5::queue::config(&QueueConfigArgs {
                target: if stage==0 {QueueConfigTarget::Tiling(None)} else {QueueConfigTarget::Fragment},
                kick_ring_va:pair.kicks[stage].low_va(),kick_ring_fw_va:pair.kicks[stage].firmware_va(),
                qid:ids[stage].qid(),install:true,completion_seed:None,context_id:pair.context.id().try_into()?,
                free_list:FreeListBinding {va:0,generation:u64::MAX,slot:u32::MAX},
                scheduler_va:pair.context.scheduler_va(),policy:POLICY,qos_slot:pair.context.qos_id(),qos_update:true,
                owner_pid:0,context_update:false,
            },fw::m5::queue::ConfigProfile{soft_fault_enabled:false,extended:false,parameter_manager:None})?;
            pair.resources.memory.graph.config.write(CONFIG[stage],config)?;
            let config_va=pair.resources.memory.graph.config.gpu_va()+CONFIG[stage] as u64;
            pair.resources.memory.graph.queues.write(ITEMS[stage],config_va)?;
        }
        Ok(pair)
    }
    pub(super) fn install(&mut self,dev:&AsahiDevice,firmware:&mut Firmware)->Result {
        firmware.init.set_m5_uma(self.resources.view.free_list_slot as usize,self.resources.uma_row)?;
        firmware.init.set_m5_pb(usize::from(self.pb_id.0),self.resources.pb_row)?;
        // Both owners and all aliases exist before either queue becomes visible.
        for id in self.ids {firmware.queues.qids.publish(id)?;}
        self.kicks[0].prepare_install()?;
        fence(Ordering::SeqCst);
        for stage in [1,0] {
            self.resources.memory.graph.queues.word(POINTERS[stage]+0x40)?.store(1,Ordering::Release);
            // M5 routes every engine's pointer list through its common work channel.
            let slot=fw::channels::WorkSlot::new(DataMaster::Compute,
                self.resources.memory.graph.queue_va(self.ids[stage].qid()),self.ids[stage].qid(),1,true)?;
            firmware.init.publish_work(POLICY.priority(),&slot)?;
        }
        firmware.primary.notify(0x0083_0000_0000_0002 | (u64::from(POLICY.priority())<<2))?;
        for _ in 0..2000 {
            if firmware.primary.state.crashed.load(Ordering::Acquire) {return Err(EIO);}
            let mut ready=true;
            for stage in 0..2 {
                for off in [0,0x30,0x40] {
                    ready &= self.resources.memory.graph.queues.word(POINTERS[stage]+off)?.load(Ordering::Acquire)==1;
                }
            }
            if ready {
                dev_info!(dev.as_ref(),"M5 render pair installed: context {}, queues [{}, {}], shared scheduler and SPTM aliases\n",
                    self.context.id(),self.ids[0].qid(),self.ids[1].qid());
                return Ok(());
            }
            fsleep(Delta::from_millis(1));
        }
        for stage in 0..2 {
            dev_err!(dev.as_ref(),"M5 render install timeout stage {} cursors [{}, {}, {}]\n",stage,
                self.resources.memory.graph.queues.word(POINTERS[stage])?.load(Ordering::Acquire),
                self.resources.memory.graph.queues.word(POINTERS[stage]+0x30)?.load(Ordering::Acquire),
                self.resources.memory.graph.queues.word(POINTERS[stage]+0x40)?.load(Ordering::Acquire));
        }
        Err(ETIMEDOUT)
    }
}

impl Pair {
    pub(super) fn draw(&mut self,dev:&AsahiDevice,firmware:&mut Firmware)->Result {
        let iterations=*crate::module_parameters::m5_render_iterations.value();
        if iterations==0 || iterations>4096 {return Err(EINVAL);}
        if self.input.is_none(){return Ok(());}
        for _ in 0..iterations {self.draw_one(dev,firmware)?;}
        let scenes=self.resources.scene_count();
        if scenes!=iterations.min(fw::buffer::SCENES as u32) {return Err(EIO);}
        dev_info!(dev.as_ref(),"M5 shared scene rotation PASS: {} slots, {} draws\n",scenes,iterations);
        let descriptors=self.descriptor_slots_seen.count_ones();
        if descriptors!=iterations.min(32) {return Err(EIO);}
        dev_info!(dev.as_ref(),"M5 shared descriptor rotation PASS: {} slots, {} draws\n",descriptors,iterations);
        if self.config_records!=2 {return Err(EIO);}
        dev_info!(dev.as_ref(),"M5 shared config reuse PASS: {} configuration records, {} draws\n",self.config_records,iterations);
        firmware.release_registration_power()?;
        let pixels=self.input.as_ref().ok_or(EINVAL)?.pixel_words();
        dev_info!(dev.as_ref(),"M5 render repeat PASS: {} draws, {} exact pixels, final stamp {:#x}, shared work-state reuse and fixed encoder workspace\n",
            iterations,pixels*iterations as usize,iterations<<8);
        Ok(())
    }

    fn draw_one(&mut self,dev:&AsahiDevice,firmware:&mut Firmware)->Result {
        use super::fw::{render::{RenderArgs,RenderStage},m5::{render as cmd,render_registers as regs,render_assembly as assembly}};
        let Some(input)=self.input.as_ref() else {return Ok(())};
        let generation=self.draws.checked_add(1).ok_or(EOVERFLOW)?;
        let stamp=generation.checked_mul(0x100).ok_or(EOVERFLOW)?;
        let verbose=generation==1;
        for n in 0..2 {
            let producer=self.resources.memory.graph.queues.word(POINTERS[n]+0x40)?.load(Ordering::Acquire);
            if [0,0x30].into_iter().any(|off|!self.resources.memory.graph.queues.word(POINTERS[n]+off)
                .is_ok_and(|word|word.load(Ordering::Acquire)==producer)){return Err(EBUSY);}
        }
        self.resources.prepare_scene(u64::from(self.draws))?;
        self.resources.memory.graph.claim(u64::from(self.draws))?;
        input.reset_result()?;
        self.resources.memory.graph.queues.bytes_mut()[0x14000..0x14120].fill(0);
        if self.current_work.is_some(){return Err(EBUSY);}
        let work_state=Arc::new(WorkStateLease::deferred(self.context.clone()),GFP_KERNEL)?;
        let work_word=work_state.acquire()?;
        let record=work_state.node_va()?;
        work_state.add_submitted_kicks(2)?;
        self.current_work=Some(work_state.clone());
        let base=self.resources.memory.graph.queues.gpu_va();
        let ordinal=u64::from(self.draws);
        let work_slot=work_state.render_slot(1)?;
        let payload=work_state.channel_payload(1,ordinal)?;
        let command_low=self.resources.memory.graph.descriptor_client(0,ordinal);
        let command_base=self.resources.memory.graph.descriptor_va(0,ordinal);
        let mut view=self.resources.view;
        view.kick_record_va=record;
        let predecessor=super::ids::OBJECTS.object()?;
        let (fragment_ids,ta_ids)=super::ids::OBJECTS.render_pair(predecessor)?;
        view.ta_ids=ta_ids;view.fragment_ids=fragment_ids;
        let stage=|n:usize|RenderStage{qid:self.ids[n].qid(),
            descriptor_va:command_low+(n*0x4000)as u64,state_word:work_word,
            stamp_va:base+0x14000+(n*0x80)as u64,aux_stamp_va:base+0x14040+(n*0x80)as u64};
        let args=RenderArgs{pass:&input.pass,resources:&view,ordinal:0,ta:stage(0),fragment:stage(1)};
        let eot=input.pass.usc_base+u64::from(input.pass.eot_offset&!7);
        let peot=input.pass.usc_base+u64::from(input.pass.partial_eot_offset&!7);
        let stage_controls=|flags|regs::M5RenderStageControls{resource_flags:flags,
            resource_control:0x200000001,ptc_max:0,work_key:0};
        let controls=regs::M5RenderControls{fragment_event_control:regs::fragment_event_control(true,1),ta:stage_controls(0x4054b),fragment:stage_controls(0x104054b),
            ta_tile_control:0x10c08860,ta_dispatch_control:0x700,fragment_raster_control:0x200000ff0200,
            fragment_tile_flags:0x200003717f,store_pipeline_2_bind:input.pass.eot_rsrc_spec<<32,
            store_pipeline_2:eot,partial_store_pipeline_2_bind:input.pass.partial_eot_rsrc_spec<<32,
            partial_store_pipeline_2:peot,partial_tile_flags:0x200000,aux_fb_flags:0x10c001,aux_fb_control:0x100000};
        let mut fields=cmd::M5RenderCommandInputs::default();
        let context_token=fw::m5::render_tail::context_token(self.context.id(),
            u64::from(self.context.render_generation())).ok_or(EINVAL)?;
        fields.ta.d1298=context_token;fields.fragment.d158=context_token;
        let usage=self.resources.memory.graph.m5_usage(ordinal)?;
        fields.ta.resolved_tail_value=usage[0];
        fields.fragment.resolved_tail_value=Some(usage[1]);
        fields.scheduler_record=record;fields.ta.sequence=u64::from(self.draws)*2+1;
        fields.fragment.sequence=u64::from(self.draws)*2;fields.fragment.pass=1;
        fields.ta.d08a4=ta_ids.predecessor;fields.fragment.d08a4=fragment_ids.predecessor;fields.ta.d1174=0;
        fields.fragment.dc68=1;fields.fragment.dc6c=1;
        fields.fragment.d0518[22][1]=0x04040404;
        fields.fragment.d08c5_flags=[1,1,0,0];
        fields.ta.d08b0=base+0x14100;
        fields.ta.diagnostic_ta_timestamp_upper=base+0x14108;
        fields.fragment.d08b0=[base+0x14110,base+0x14118];
        fields.ta.linked_3d=cmd::M5TaLinked3d{queue_index:u32::from(self.ids[1].qid()),stamp,
            completion:0,flag:0};
        fields.ta.completion=cmd::M5StageCompletion{stamp_addresses:[args.ta.stamp_va,args.ta.aux_stamp_va],
            stamp,queue_index:u32::from(self.ids[0].qid()),ordinal:0,has_wait:false,
            completion:0,key:u64::from(ta_ids.current)};
        fields.fragment.completion=cmd::M5StageCompletion{stamp_addresses:[args.fragment.stamp_va,args.fragment.aux_stamp_va],
            stamp,queue_index:u32::from(self.ids[1].qid()),ordinal:0,has_wait:false,
            completion:0,key:u64::from(fragment_ids.current)};
        self.resources.bind_commands(&mut fields);
        regs::apply_pass(&args,&controls,&mut fields)?;
        let extra=self.resources.extra;
        let encoded=assembly::encode_into(self.resources.memory.graph.m5_storage(ordinal)?,command_low,&args,&controls,
            extra,regs::FragmentDispatch{words:[0;2]},&fields,&mut self.workspace)?;
        assembly::bind_work_slot(self.resources.memory.graph.m5_storage(ordinal)?,work_slot)?;
        assembly::link_completions(self.resources.memory.graph.m5_storage(ordinal)?,
            self.kicks.each_ref().map(kick::Queue::timestamp),payload,false)?;
        if verbose {dump(dev,"command-ta",&self.resources.memory.graph.descriptors,self.resources.memory.graph.offset(0,ordinal),0x9a0)?;}
        if verbose {dump(dev,"command-fragment",&self.resources.memory.graph.descriptors,self.resources.memory.graph.offset(1,ordinal),0x2240)?;}
        if verbose {self.resources.dump(dev)?;}
        let dependency=fw::kick::KickDependency::new(self.ids[0].qid(),self.kicks[0].timestamp()).ok_or(EINVAL)?;
        let mut configs=[None;2];
        for n in 0..2 {
            let first=self.configs[n].is_none();
            let target=if n==0 {QueueConfigTarget::Tiling(Some(TilingBinding{
                pb_va:view.pb_va,pb_token:1,pb_slot:view.pb_slot,fragment_qid:self.ids[1].qid()}))}else{QueueConfigTarget::Fragment};
            let conf=fw::m5::queue::config(&QueueConfigArgs{target,
                kick_ring_va:self.kicks[n].low_va(),kick_ring_fw_va:self.kicks[n].firmware_va(),
                qid:self.ids[n].qid(),install:false,completion_seed:first.then(||self.kicks[n].timestamp().prev()
                    .completion_value(payload.wrapping_sub(1))),context_id:view.context_id,
                free_list:FreeListBinding{va:view.free_list_control_va,generation:1,slot:view.free_list_slot},
                scheduler_va:self.context.scheduler_va(),policy:POLICY,qos_slot:self.context.qos_id(),qos_update:false,
                owner_pid:0,context_update:first},fw::m5::queue::ConfigProfile{
                soft_fault_enabled:true,extended:false,parameter_manager:if n==0 {Some(view.pb_slot)}else{None}})?;
            configs[n]=Some(conf);
            let barriers=if n==0 {&[][..]}else{core::slice::from_ref(&dependency)};
            let types=if n==0 {&[1u8][..]}else{&[1u8,3][..]};
            let entry=fw::kick::KickArgs{qid:self.ids[n].qid(),timestamp:self.kicks[n].timestamp(),
                parent:self.kicks[n].parent(),barriers,dependency_types:Some(types),
                descriptor_va:command_base+(n*0x4000)as u64,queue_va:self.resources.memory.graph.queue_va(self.ids[n].qid()),
                qos:fw::kick::KickQos{slot:self.context.qos_id(),class:if n==0{12}else{24}},mcache:None,event_mask:[0;4],
                register_arrays:encoded.rce[n],compute_scratch:false,priority:POLICY.priority()};
            let prepared=self.kicks[n].prepare_entry(&entry)?;
            prepared.prepare_install()?;
            let submitted_stamp=prepared.commit();
            let words=self.kicks[n].read_entry_words(submitted_stamp)?;
            if verbose {dump_words(dev,if n==0 {"kick-ta"}else{"kick-fragment"},&words);}
            if verbose {dump(dev,if n==0 {"queue-ta"}else{"queue-fragment"},&self.resources.memory.graph.queues,self.resources.memory.graph.queue_offset(self.ids[n].qid()),0x80)?;}
            if verbose {dump(dev,if n==0 {"pointers-ta"}else{"pointers-fragment"},&self.resources.memory.graph.queues,POINTERS[n],0x80)?;}
        }
        let present=core::array::from_fn::<_,2,_>(|n|publication::required(self.configs[n],configs[n].unwrap()));
        let lane=|n:usize|Lane::new(&self.resources.memory.graph,n,ordinal,
            self.resources.memory.graph.queues.word(POINTERS[n]+0x40)?.load(Ordering::Relaxed),
            configs[n].ok_or(EINVAL)?,present[n],self.kicks[n].timestamp());
        let lanes=[lane(0)?,lane(1)?];
        let targets=lanes.each_ref().map(Lane::target);
        let slot=|n:usize|fw::channels::WorkSlot::new(DataMaster::Compute,
            self.resources.memory.graph.queue_va(self.ids[n].qid()),self.ids[n].qid(),
            targets[n].try_into()?,false);
        let outer=[slot(1)?,slot(0)?];
        for lane in &lanes {lane.prepare();}
        if verbose {
            for n in 0..2 {
                let slot=ordinal as usize % 32;
                dump(dev,if n==0 {"config-ta"}else{"config-fragment"},&self.resources.memory.graph.config,CONFIG[n]+slot*CONFIG_STRIDE,0xc0)?;
                dump(dev,if n==0 {"notification-ta"}else{"notification-fragment"},&self.resources.memory.graph.queues,ANNOUNCE[n]+slot*ANNOUNCE_STRIDE,0x20)?;
            }
        }
        if verbose {
            dump(dev,"scheduler",&self.context.scheduler_owner(),0,0x4000)?;
            work_state.with_node(|object,offset| {
                dump(dev,"work-node",object,offset,0xc8)?;
                dump(dev,"work-head",object,
                    (self.context.work_head_va()-object.gpu_va()) as usize,0x18)
            })?;
        }
        fence(Ordering::SeqCst);
        let submission=if verbose {
            fw::m5::queue::HardwareSubmission::new(
                (self.ids[1].qid(),self.kicks[1].low_va()),
                (self.ids[0].qid(),self.kicks[0].low_va()),POLICY.priority(),true)?
        } else {fw::m5::queue::HardwareSubmission::advance_pair(
            self.ids[1].qid(),self.ids[0].qid())?};
        // Prepare and inspect the entire graph before acquiring the short
        // publication power hold. No allocation or logging separates it
        // from the host-port transaction.
        if verbose {firmware.acquire_registration_power()?;}
        let before=if verbose {Some(firmware.bridge.regs.m5_queue_configurations([self.ids[0].qid(),self.ids[1].qid()])?)}else{None};
        let owners=core::array::from_fn::<_,2,_>(|n|qos::Owner {
            qid:self.ids[n].qid(),qos:self.context.qos_id(),data_master:n as u8,
        });
        let qos_view=qos::View::new(firmware.init.qos()?)?;
        let fragment=firmware.queues.accounting.publish(&qos_view,owners[1],
            self.context.scheduler_va(),POLICY,qos::clock)?;
        if let Err(error)=firmware.queues.accounting.publish(&qos_view,owners[0],
            self.context.scheduler_va(),POLICY,qos::clock) {
            firmware.queues.accounting.cancel(&qos_view,fragment)?;
            return Err(error);
        }
        // Both identities and counters precede the first hardware-visible kick.
        // Once a port write is possible, retain accounting until exact retirement.
        self.context.mark_published();
        let deferred=self.draws%2==0;
        let next=firmware.submit_m5_render_pair(POLICY.priority(),&outer,&submission,||Ok(()),|| {
            for n in [1,0] {lanes[n].publish_kick();}
        },deferred)?;
        if deferred {
            firmware.init.publish_retained_work_pair(POLICY.priority(),&outer[0],&outer[1],next)?;
        }
        if let Some(before)=before {
            let after=firmware.bridge.regs.m5_queue_configurations([self.ids[0].qid(),self.ids[1].qid()])?;
            dev_info!(dev.as_ref(),"M5 host queue configurations: before {:x?}, after {:x?}\n",before,after);
        }
        firmware.primary.notify(0x0083_0000_0000_0002|(u64::from(POLICY.priority())<<2))?;
        if verbose {dump(dev,"qos",firmware.init.qos()?,0,core::mem::size_of::<fw::qos::Qos>())?;}
        if verbose {dev_info!(dev.as_ref(),"M5 render submitted: {}x{}, context {}, queues [{}, {}]\n",
            input.pass.width,input.pass.height,self.context.id(),self.ids[0].qid(),self.ids[1].qid());}
        let mut complete=false;
        for poll in 0..5000 {
            if poll%256==0 {
                for role in [super::Role::Primary,super::Role::Secondary] {
                    let mut events=super::channel::Events::new(role);
                    while let Some(event)=events.next(dev.as_ref(),&firmware.init)? {
                        if verbose || matches!(event,fw::channels::Event::Restart{..}) {
                            dev_info!(dev.as_ref(),"M5 render {:?} event {:?}\n",role,event);
                        }
                        if matches!(event,fw::channels::Event::Restart{..}) {
                            dev_info!(dev.as_ref(),"M5 raw fault [selector, info, address]: {:?}\n",firmware.bridge.regs.m5_fault_registers());
                            firmware.init.dump_m5_failure(dev)?;
                        }
                    }
                    firmware.rings.drain_trace(dev.as_ref(),&firmware.init,role);
                    firmware.rings.drain_statistics(dev.as_ref(),&firmware.init,role);
                }
            }
            if firmware.primary.state.crashed.load(Ordering::Acquire){break;}
            complete=work_state.with_node(|object,offset| {
                Ok(object.word(offset+0xc)?.load(Ordering::Acquire)==2
                    &&[0x28,0x4c,0x70,0x94].into_iter().all(|off|
                        object.word(offset+off).is_ok_and(|word|word.load(Ordering::Acquire)==0)))
            })? && self.resources.memory.graph.queues.word(0x14000)?.load(Ordering::Acquire)==stamp
                &&self.resources.memory.graph.queues.word(0x14080)?.load(Ordering::Acquire)==stamp;
            complete &= (0..2).all(|n|[0,0x30,0x40].into_iter().all(|off|
                self.resources.memory.graph.queues.word(POINTERS[n]+off).is_ok_and(|word|word.load(Ordering::Acquire)==targets[n])));
            if complete{break;}
            fsleep(Delta::from_millis(1));
        }
        if complete {
            firmware.queues.accounting.complete(&qos::View::new(firmware.init.qos()?)?,&owners)?;
        }
        // The image is read only after observing firmware completion writes.
        unsafe{core::arch::asm!("dmb oshld",options(nostack,preserves_flags));}
        let (wrong,untouched,first)=input.result()?;
        if verbose || generation%64==0 || !complete || wrong!=0 {
        dev_info!(dev.as_ref(),"M5 render result: complete {}, wrong {}, untouched {}, first {:#x}, stamps [{:#x}, {:#x}], completed {}\n",
            complete,wrong,untouched,first,self.resources.memory.graph.queues.word(0x14000)?.load(Ordering::Acquire),
            self.resources.memory.graph.queues.word(0x14080)?.load(Ordering::Acquire),work_state.completed_kicks()?);
        dev_info!(dev.as_ref(),"M5 render stage samples: [{:#x}, {:#x}, {:#x}, {:#x}], cursors [{}, {}, {}] [{}, {}, {}]\n",
            self.resources.memory.graph.queues.dword(0x14100)?.load(Ordering::Acquire),self.resources.memory.graph.queues.dword(0x14108)?.load(Ordering::Acquire),
            self.resources.memory.graph.queues.dword(0x14110)?.load(Ordering::Acquire),self.resources.memory.graph.queues.dword(0x14118)?.load(Ordering::Acquire),
            self.resources.memory.graph.queues.word(POINTERS[0])?.load(Ordering::Acquire),self.resources.memory.graph.queues.word(POINTERS[0]+0x30)?.load(Ordering::Acquire),self.resources.memory.graph.queues.word(POINTERS[0]+0x40)?.load(Ordering::Acquire),
            self.resources.memory.graph.queues.word(POINTERS[1])?.load(Ordering::Acquire),self.resources.memory.graph.queues.word(POINTERS[1]+0x30)?.load(Ordering::Acquire),self.resources.memory.graph.queues.word(POINTERS[1]+0x40)?.load(Ordering::Acquire));
        }
        if !complete || wrong!=0 {
            firmware.init.dump_m5_failure(dev)?;
            work_state.with_node(|object,offset|dump(dev,"work-node",object,offset,0xc8))?;
            for (label,offset,count) in [("queue-ta",0,0x80),("queue-fragment",0x100,0x80)] {
                for i in (0..count).step_by(4){let value=self.resources.memory.graph.queues.word(offset+i)?.load(Ordering::Acquire);
                    if value!=0{dev_info!(dev.as_ref(),"M5 render {} +{:#x}={:#x}\n",label,i,value);}}
            }
            return Err(EIO);
        }
        self.resources.retire_scene(u64::from(self.draws))?;
        self.configs=configs;
        self.config_records+=present.into_iter().filter(|present|*present).count() as u32;
        self.descriptor_slots_seen |= 1 << (self.draws % 32);
        self.draws=generation;
        self.current_work.take();
        Ok(())
    }
}

pub(super) fn dump(dev:&AsahiDevice,label:&str,object:&KernelObject,start:usize,bytes:usize)->Result {
    dump_device(dev.as_ref(), label, object, start, bytes)
}

pub(super) fn dump_device(dev:&kernel::device::Device,label:&str,object:&KernelObject,start:usize,bytes:usize)->Result {
    dev_info!(dev,"M5 dump-begin {} {:#x}\n",label,bytes);
    let mut zero_start=0;
    let mut zero_bytes=0;
    for at in (start..start+bytes).step_by(32) {
        let v=[object.dword(at)?.load(Ordering::Relaxed),object.dword(at+8)?.load(Ordering::Relaxed),
            object.dword(at+16)?.load(Ordering::Relaxed),object.dword(at+24)?.load(Ordering::Relaxed)];
        if v!=[0;4] {
            if zero_bytes!=0 {
                dev_info!(dev,"M5 dump-zero {} {:#x} {:#x}\n",label,zero_start,zero_bytes);
                zero_bytes=0;
            }
            dev_info!(dev,"M5 dump {} {:#x} {:016x} {:016x} {:016x} {:016x}\n",
                label,at-start,v[0],v[1],v[2],v[3]);
        } else {
            if zero_bytes==0 {zero_start=at-start;}
            zero_bytes+=32usize.min(start+bytes-at);
        }
    }
    if zero_bytes!=0 {
        dev_info!(dev,"M5 dump-zero {} {:#x} {:#x}\n",label,zero_start,zero_bytes);
    }
    dev_info!(dev,"M5 dump-end {} {:#x}\n",label,bytes);
    Ok(())
}

fn dump_words(dev:&AsahiDevice,label:&str,words:&[u32]) {
    for (index,w) in words.chunks_exact(8).enumerate() {
        let value=|i:usize|u64::from(w[i])|(u64::from(w[i+1])<<32);
        dev_info!(dev.as_ref(),"M5 dump {} {:#x} {:016x} {:016x} {:016x} {:016x}\n",
            label,index*32,value(0),value(2),value(4),value(6));
    }
}

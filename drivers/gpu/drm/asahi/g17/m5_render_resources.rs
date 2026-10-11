// SPDX-License-Identifier: GPL-2.0-only
//! M5 qualification over the normal render-memory owner.
use super::{buffer::target, context::Context, fw::{self, m5::{pool,
    render::M5RenderCommandInputs, render_registers::TaExtra},
    render::{RenderPass, RenderResources}}, object::{Allocator, Pool},
    queue::render::memory::Memory};
use crate::{driver::AsahiDevice, mmu};
use kernel::{prelude::*, sync::Arc};

const PB_BLOCK:usize=0x80000;
const SCENE_TABLE:usize=0x400;
const SCENE:usize=SCENE_TABLE+pool::SCENE_STRIDE;

pub(super) struct Resources {
    aliases:KVec<mmu::KernelMapping>,
    pub(super) memory:KBox<Memory>,
    layout:target::Layout,
    pass:RenderPass,
    pool:Arc<super::freelist::RenderPool>,
    scenes_seen:u128,
    pub(super) view:RenderResources,
    pub(super) extra:TaExtra,
    pub(super) uma_row:[u64;8],
    pub(super) pb_row:[u32;4],
}
impl Resources {
    pub(super) fn new(alloc:&Allocator<'_>,objects:&Arc<Pool>,context:Arc<Context>,
        qids:[u8;2],pass:&RenderPass,clusters:u32,pb_slot:u8,
        private_pool:Arc<super::freelist::RenderPool>,metrics:super::buffer::MetricsLease)->Result<Self> {
        if !alloc.uat.is_sptm() {return Err(EINVAL);}
        // Explicit single-group qualification policy. Physical cluster count
        // still sizes TPC storage. Normal rendering selects its own policy.
        let active_groups=1;
        let layout=target::Layout::new(pass,clusters)?;
        let blocks=layout.tvb_blocks;
        let initial_bytes=blocks.checked_mul(PB_BLOCK).ok_or(EOVERFLOW)?;
        let layout=layout.with_m5_slices(pass,active_groups,64,initial_bytes)?;
        let (mut memory,aliases)=Memory::new(alloc,objects,context,qids,&layout,metrics,
            (pb_slot,1),1,0,0)?;
        let target_addresses=memory.target.prepare(0,&layout)?;
        let extra=memory.target.m5_extra(&layout)?;
        let [deflake,ta_status,fragment_status,fragment_status_fw,auxiliary]=memory.generation(0)?.addresses();
        let (scratch,metric)=memory.manager.scene_registers(fw::buffer::FIRST_SCENE)?;
        let uma_slot=u32::from(private_pool.id());
        let uma_row=private_pool.m5_descriptor()?;
        let pb_row=fw::buffer::reload_descriptor([0;4],memory.manager.page_list_va(),(blocks*16)as u32);
        let view=RenderResources{context_id:0,context_generation:0,pb_slot:u32::from(pb_slot),free_list_slot:uma_slot,
            free_list_control_va:private_pool.control_va(),kick_record_va:0,pb_va:memory.manager.state_va(),scene_va:memory.manager.scene_va(fw::buffer::FIRST_SCENE)?,
            scene_end_va:memory.manager.trailer_va(),
            pm_scratch:scratch,pm_metric:metric,tilemap_va:target_addresses.tilemap,
            layermeta_va:target_addresses.layer,heapmeta_va:target_addresses.heap,
            tpc_va:memory.target.tpc_va(),tpc_size:layout.tpc_size as u64,deflake_va:deflake,ta_status_va:ta_status,
            fragment_status_va:fragment_status,fragment_status_fw_va:fragment_status_fw,aux_fb_va:auxiliary,timestamp_va:0,
            ta_ids:fw::ObjectIds{predecessor:0,current:0},fragment_ids:fw::ObjectIds{predecessor:0,current:0},unk_2104:0,unk_210c:0};
        Ok(Self{aliases,memory,layout,pass:*pass,pool:private_pool,scenes_seen:0,view,extra,uma_row,pb_row})
    }
    pub(super) fn prepare_scene(&mut self,ordinal:u64)->Result {
        let target=self.memory.target.prepare(ordinal,&self.layout)?;
        self.view.tilemap_va=target.tilemap;self.view.layermeta_va=target.layer;
        self.view.heapmeta_va=target.heap;
        let generation=self.memory.generation(ordinal)?;
        generation.prepare(ordinal,&self.pass)?;
        let [deflake,ta_status,fragment_status,fragment_fw,auxiliary]=generation.addresses();
        self.view.deflake_va=deflake;self.view.ta_status_va=ta_status;
        self.view.fragment_status_va=fragment_status;self.view.fragment_status_fw_va=fragment_fw;
        self.view.aux_fb_va=auxiliary;
        let scene=self.memory.manager.prepare_scene(ordinal)?;
        self.memory.manager.claim_scene(ordinal,scene)?;
        let (scratch,metric)=self.memory.manager.scene_registers(scene)?;
        self.view.scene_va=self.memory.manager.scene_va(scene)?;
        self.view.pm_scratch=scratch;
        self.view.pm_metric=metric;
        self.memory.manager.prepare_submission()?.commit();
        self.pool.prepare_submission(2)?.commit();
        self.scenes_seen|=1u128<<scene;
        Ok(())
    }
    pub(super) fn retire_scene(&mut self,ordinal:u64)->Result {
        if self.pool.counters()? != [2 * (ordinal + 1); 2] { return Err(EIO); }
        self.memory.retire(ordinal)
    }
    pub(super) fn scene_count(&self)->u32 {self.scenes_seen.count_ones()}
    pub(super) fn dump(&self,dev:&AsahiDevice)->Result {
        super::m5_render_test::dump(dev,"pb",self.memory.manager.graph(),0,0xc0)?;
        super::m5_render_test::dump(dev,"uma",self.pool.control_object(),0,0x100)?;
        super::m5_render_test::dump(dev,"scene",self.memory.manager.graph(),SCENE,pool::SCENE_STRIDE)?;
        super::m5_render_test::dump(dev,"scene-trailer",self.memory.manager.graph(),
            SCENE_TABLE+pool::SCENE_TABLE_ENTRIES*pool::SCENE_STRIDE,pool::SCENE_TRAILER_SIZE)
    }
    pub(super) fn bind_commands(&self,inputs:&mut M5RenderCommandInputs) {
        inputs.ta.pb_slot=self.view.pb_slot;
        inputs.ta.pb_state=self.view.pb_va;inputs.fragment.pm_state=self.view.pb_va;
        inputs.ta.pm_scene=self.view.scene_va;inputs.fragment.pm_scene=self.view.scene_va;
        inputs.ta.pm_b8=self.view.scene_end_va;inputs.fragment.pm_b8=self.view.scene_end_va;
        inputs.ta.uma_state=self.view.free_list_control_va;inputs.fragment.uma_state=self.view.free_list_control_va;
        inputs.ta.d1444=1;inputs.fragment.dbc4=1;
    }
}

/// A fixed geometry used only to qualify owned storage and paired queues.
/// Program addresses are supplied by the later draw packet.
pub(super) fn qualification_pass() -> RenderPass {
    use fw::render::{UscProgram,ZlsSurface};
    let surface=ZlsSurface{va:0,compression_va:0,stride:0,compression_stride:0};
    let program=UscProgram{offset:0,rsrc_spec:0};
    RenderPass{width:256,height:256,layers:1,utile_width:32,utile_height:32,samples_log2:0,
        sample_size:16,process_empty_tiles:true,msaa_control:0x88,ppp_control:0x202,
        vdm_va:0,scissor_va:0,depth_bias_va:0,depth_bias_is_int:false,few_primitives:true,
        fragment_barriers:false,occlusion_query_va:0,sampler_heap_va:0,sampler_count:0,
        zls_control:0,depth_dimensions:0,depth:surface,stencil:surface,depth_clear:0x3f800000,
        stencil_clear:0,merge_upper_x:0x3bddb3d9,merge_upper_y:0x3bddb3d9,usc_base:0,
        bg:program,partial_bg:program,eot_offset:0,eot_rsrc_spec:0,partial_eot_offset:0,
        partial_eot_rsrc_spec:0,rsrc_spec_hi:true}
}

/// Exercise initial allocation, larger demand and warm reuse before any GPU
/// reference. Synthetic programs reserve storage independently of the draw.
pub(super) fn qualify_pool_growth(alloc: &Allocator<'_>, context: &Arc<Context>,
    ids: &Arc<super::freelist::RenderIds>, global: &mmu::Vm,
    policy: &super::render_resources::M5Policy) -> Result<Arc<super::freelist::RenderPool>> {
    use kernel::uapi::drm_asahi_render_program as Program;
    let plan = |primary_bytes| {
        let programs = super::render_resources::Programs::from_entries(&[
            Program::default(),
            Program { stage: 1, primary_bytes, imageblock_bytes: 8192, ..Program::default() },
        ])?;
        policy.plan(&programs, context.policy().priority())
    };
    let initial = plan(0)?;
    let required = plan(1040)?;
    let old = context.render_pool(alloc, ids, global, Some(initial))?;
    let next = context.render_pool(alloc, ids, global, Some(required))?;
    if Arc::ptr_eq(&old, &next) || old.fits(Some(required.geometry))
        || !next.fits(Some(required.geometry)) || old.counters()? != [0; 2] {
        return Err(EIO);
    }
    let reused = context.render_pool(alloc, ids, global, Some(initial))?;
    if !Arc::ptr_eq(&next, &reused) { return Err(EIO); }
    dev_info!(alloc.dev.as_ref(), "M5 render pool allocation/growth/reuse PASS: {} -> {} bytes; old backing retained during replacement\n",
        initial.sizes.ideal, required.sizes.ideal);
    Ok(next)
}

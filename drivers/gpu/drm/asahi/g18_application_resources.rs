// SPDX-License-Identifier: GPL-2.0-only OR MIT
use super::*;
use crate::g18_application_runtime::Frame;
use crate::g17_resources::g18_graphics_job::application::ApplicationJob;

impl G18FirmwareMemory {
    pub(crate) fn application_aliases(&mut self, vm: &mmu::Vm) -> Result<KVec<mmu::KernelMapping>> {
        self.uma_pool.application_aliases(&self.uat,vm)
    }

    pub(crate) fn audit_application(&mut self, dev: &AsahiDevice, job: &mut ApplicationJob) -> Result {
        let frame=job.frame();let context=u32::from(job.job.context_id());
        let pb=job.job.parameter_buffer().verify_resident(&mut self.pbdesc,context,frame.generation)?;
        let uma=self.uma_pool.verify_application_descriptor(&mut self.uma,j873_sma_device_words(2,6)?,frame.generation==1)?;
        let pages=self.uma_pool.audit_render_mappings(&self.uat,job.job.vm())?;
        job.audit(dev,&self.uat,&pb,&uma,pages)?;
        if frame.generation==1 {
            let state=self.uma_pool.render_state()?;let mut row=[0u8;128];
            for (out,word) in row.chunks_exact_mut(8).zip(uma) {out.copy_from_slice(&word.to_le_bytes());}
            for (name,bytes) in [("uma-state",state.as_slice()),("uma-row",row.as_slice())] {
                dev_info!(dev.as_ref(),"G18_RENDER_DIGEST name={} fnv={:#018x}\n",name,crate::g18_graphics_payload::fnv64(bytes));
            }
        }
        self.completion_masks=[0;2];Ok(())
    }

    pub(crate) fn publish_application_outer(&mut self, records: &[[u8;24];2], frame: Frame) -> Result {
        let before=frame.outer_before();
        if self.control_counters()?!=[[2;3],[1;3]] || self.compute_outer_counters()?!=[before;3] {return Err(EBUSY);}
        for (index,record) in records.iter().enumerate() {
            let qid=if index==0 {10} else {9};
            if record[22]!=qid || record[23]!=1 || record[16..20]!=2u32.to_le_bytes()
                || record[20..22]!=(frame.inner_after() as u16).to_le_bytes()
                || record[8..16]!=(crate::g18_address_map::GRAPHICS_QUEUE_HIGH+if qid==9 {0} else {0x100}).to_le_bytes() {return Err(EINVAL);}
        }
        self.shared.with_bytes_mut(|out| {
            for (index,record) in records.iter().enumerate() {
                let start=init::G18G_26A428_WORK_RING_BUNDLE_OFFSETS[8]+((before as usize+index)%256)*24;
                out.get_mut(start..start+24).ok_or(EINVAL)?.copy_from_slice(record);
            } Ok(())
        })?;
        if self.compute_outer_counters()?!=[before;3] {return Err(EBUSY);}
        fence(Ordering::SeqCst);
        self.private.write_u32(PRIVATE_BASE+(init::G18G_26A428_PRIVATE_STATE_GRIDS[0]
            +init::WORK_STATE_GRID_OFFSETS[8]+0x20) as u64,frame.outer_after())?;
        fence(Ordering::SeqCst);Ok(())
    }
}

// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Stage10 owner: synchronous DRM boundary, shallow worker stack, sticky fault.
use super::*;
use core::{ops::Range,sync::atomic::AtomicI32};
use crate::{g18_application::{G18ApplicationRender,G18RenderJobSink,G18RenderRetirement},
    g18_drm::{self,G18DrmRuntime},gem,
    g17_resources::g18_graphics_job::application::ApplicationJob};

pub(crate) struct ApplicationOwner {
    session: Arc<Mutex<Retained>>,
    failed: Arc<AtomicBool>,
}

impl ApplicationOwner {
    fn work<F>(&self, task: F) -> Result
        where F: FnOnce(&mut G18Session)->Result + Send + 'static {
        if self.failed.load(Ordering::Acquire) {return Err(EIO);}
        let session=self.session.clone();let failed=self.failed.clone();
        let ticket=Arc::new(AtomicI32::new(1),GFP_KERNEL)?;let done=ticket.clone();
        kernel::workqueue::system_long().try_spawn(GFP_KERNEL,move || {
            let mut owner=session.lock();
            let result=if failed.load(Ordering::Acquire) {Err(EIO)} else {
                owner.0.as_mut().ok_or(EIO).and_then(|session|task(session))
            };
            if let Err(error)=result {
                failed.store(true,Ordering::Release);
                if let Some(session)=owner.0.as_mut() {
                    if let Some(job)=session.application_job.as_mut() {job.quarantine();}
                    dev_err!(session.device(),"G18_APP_FAIL error={:?} owners=retained no-retry\n",error);
                    session.dump_fault_evidence("application worker");
                    if let Some(memory)=session.memory.as_ref() {memory.log_trace_journal(&session.dev);}
                }
                let _=owner.stop("application fault");
            }
            done.store(result.map_or_else(|e|e.to_errno(),|()|0),Ordering::Release);
        })?;
        loop {
            let status=ticket.load(Ordering::Acquire);
            if status!=1 {return if status==0 {Ok(())} else {Err(Error::from_errno(status))};}
            fsleep(Delta::from_millis(1));
        }
    }
}

impl G18Runtime for ApplicationOwner {
    fn shutdown(&self) {
        self.failed.store(true,Ordering::Release);
        let _=self.session.lock().stop("DRM unbind");
    }
}
impl G18DrmRuntime for ApplicationOwner {
    fn is_crashed(&self)->bool {
        self.failed.load(Ordering::Acquire)
            || self.session.lock().0.as_ref().map_or(true,|s|s.any_crashed())
    }
    fn new_user_vm(&self,id:u64,kernel_range:Range<u64>)->Result<mmu::Vm> {
        if self.failed.load(Ordering::Acquire) {return Err(EIO);}
        self.session.lock().0.as_ref().ok_or(EIO)?.uat.new_vm(id,kernel_range)
    }
    fn map_timestamp_buffer(&self,bo:gem::ObjectRef,range:Range<usize>)->Result<mmu::KernelMapping> {
        if self.failed.load(Ordering::Acquire) {return Err(EIO);}
        g18_drm::map_timestamp_buffer_in(&self.session.lock().0.as_ref().ok_or(EIO)?.uat,bo,range)
    }
}
impl G18RenderJobSink for ApplicationOwner {
    fn submit_render(&self,vm:&mmu::Vm,render:&G18ApplicationRender)->Result<G18RenderRetirement> {
        let render=KBox::new(*render,GFP_KERNEL)?;let job_vm=vm.clone();
        let result=self.work(move |session|session.submit_application(&job_vm,&render));
        if let Err(error)=result {vm.status().record(error.to_errno());return Err(error);}
        // K8 uses the adapter's architectural-counter bracket until firmware
        // timestamp units and stage boundaries have hardware qualification.
        Ok(G18RenderRetirement {stage_ticks:None})
    }
}

pub(crate) fn start_drm(pdev:&platform::Device<Core>,probe:T8152Probe)
    ->Result<(Arc<dyn G18Runtime>,Arc<dyn G18DrmRuntime>)> {
    if probe.stage!=STAGE_DRM {return Err(EINVAL);}
    let retained=prepare_firmware_session(pdev,probe)?;
    let session=Arc::pin_init(kernel::new_mutex!(retained,"G18 application session"),GFP_KERNEL)?;
    let owner=Arc::new(ApplicationOwner {session,failed:Arc::new(AtomicBool::new(false),GFP_KERNEL)?},GFP_KERNEL)?;
    owner.work(|session|session.run())?;
    dev_info!(pdev.as_ref(),"G18_DRM_READY firmware=live graphics=on-first-submit timestamps=K8\n");
    Ok((owner.clone(),owner))
}

impl G18Session {
    #[inline(never)]
    fn submit_application(&mut self,vm:&mmu::Vm,render:&G18ApplicationRender)->Result {
        self.check_faults("application prepare")?;
        if self.application_job.is_none() {
            self.application_job=Some(ApplicationJob::new(self.render_job.take().ok_or(EIO)?)?);
        }
        let job=self.application_job.as_mut().ok_or(EIO)?;
        job.prepare(&self.dev,&self.uat,vm,render)?;
        if job.pool_aliases.is_empty() {
            job.pool_aliases=self.memory.as_mut().ok_or(EIO)?.application_aliases(vm)?;
        }
        let frame=job.frame();let pass=job.pass()?;
        if frame.generation==1 {
            for phase in 1..=3 {
                self.check_faults("application binding")?;
                let queues=self.application_job.as_mut().ok_or(EIO)?.job.queues();
                let records=match phase {1=>queues.install_records(&pass)?,2=>queues.scheduler_records(&pass)?,_=>queues.pool_records(&pass)?};
                self.memory.as_mut().ok_or(EIO)?.publish_outer_records(&records)?;
                Pin::new(&mut self.roles[G18Role::Gfx.index()].rtkit).send_message(EP_GFX_INTERRUPTS,0x0083_0000_0000_000a)?;
                let mut passed=false;
                for _ in 0..CONTROL_POLLS {
                    self.check_faults("application binding")?;
                    if self.memory.as_mut().ok_or(EIO)?.compute_outer_counters()?==[phase*2;3]
                        && self.application_job.as_mut().ok_or(EIO)?.job.queues().stage_gate(phase,&pass)? {passed=true;break;}
                    fsleep(Delta::from_millis(1));
                }
                if !passed {return Err(ETIMEDOUT);}
                dev_info!(self.device(),"G18_APP_BIND_PASS phase={}\n",phase);
            }
        }
        let job=self.application_job.as_mut().ok_or(EIO)?;
        if job.job.parameter_buffer().count_submitted_render()?!=frame.generation {return Err(EIO);}
        self.memory.as_mut().ok_or(EIO)?.audit_application(&self.dev,job)?;
        job.job.queues().prepare_application_pass(&self.dev,&pass,frame)?;
        self.check_faults("application pre-kick")?;
        let job=self.application_job.as_mut().ok_or(EIO)?;
        let roots=job.publish()?;
        let records=job.job.queues().publish_application_pass(frame)?;
        self.memory.as_mut().ok_or(EIO)?.publish_application_outer(&records,frame)?;
        Pin::new(&mut self.roles[G18Role::Gfx.index()].rtkit).send_message(EP_GFX_INTERRUPTS,0x0083_0000_0000_000a)?;
        if frame.generation<=3 {
            dev_info!(self.device(),"G18_APP_KICK frame={} context={} roots={:x?} order=3D,TA stamp={:#x}\n",frame.generation,pass.context_id,roots,frame.stamp());
        }
        for step in 0..10_000 {
            self.check_faults("application completion")?;
            let memory=self.memory.as_mut().ok_or(EIO)?;
            let outer=memory.compute_outer_counters()?;let masks=memory.completion_masks();
            let job=self.application_job.as_mut().ok_or(EIO)?;
            let inner=job.job.queues().counters()?;let stamps=job.job.stamps()?;
            let scheduler=job.job.scheduler_snapshot()?;let list=job.job.queues().completion_list()?;
            if outer==[frame.outer_after();3] && inner==[[frame.inner_after();3];2]
                && stamps==[frame.stamp();2] && scheduler.submitted==frame.submitted()
                && scheduler.completed==frame.submitted() && scheduler.linked==0
                && list==[0,job.job.queues().job_list()] && masks[0]&0x600==0x600 {
                self.check_faults("application retired")?;
                self.application_job.as_mut().ok_or(EIO)?.retired();
                if frame.generation<=3 || frame.generation%256==0 {
                    dev_info!(self.device(),"G18_APP_FRAME_PASS frame={} stamp={:#x} scheduler={} outer={} inner={}\n",
                        frame.generation,frame.stamp(),frame.submitted(),frame.outer_after(),frame.inner_after());
                }
                return Ok(());
            }
            if step%1000==999 {dev_info!(self.dev.as_ref(),"G18_APP_WAIT frame={} ms={} outer={:?} inner={:?} stamps={:x?} masks={:x?} scheduler={:?}\n",
                frame.generation,step+1,outer,inner,stamps,masks,scheduler);}
            fsleep(Delta::from_millis(1));
        }
        Err(ETIMEDOUT)
    }
}

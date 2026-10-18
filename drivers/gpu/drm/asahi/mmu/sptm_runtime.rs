// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Runtime ownership for provider-managed roots. Queues share a VM's one
//! hardware binding, but retain independent scheduler and retirement owners.

use super::*;
use crate::g17::context::identity::WorkStates;
use kernel::platform;

struct Contexts {
    used: u64,
    generations: [u64; UAT_NUM_CTX],
    /// Published commands naming each identity that have not yet retired
    /// exactly (native context ID manager reference counts).
    pins: [u32; UAT_NUM_CTX],
    /// The VM each identity is bound to. Only VMs with live queues appear.
    owners: [Option<Vm>; UAT_NUM_CTX],
    /// That VM's commands present on physical queues (see [`Binding::activity`]).
    activity: [Option<Arc<AtomicU32>>; UAT_NUM_CTX],
    steals: u64,
    /// Steal candidates with no pin whose VM still had commands on physical
    /// queues. Nonzero means a command ran without its pin.
    unpinned_active: u64,
}





enum Bound {
    Pinned(ContextPin),
    Unbound,

}

#[pin_data]
pub(super) struct UatState {
    transport: Arc<sptm_transport::Transport>,
    pub(super) available: Arc<crate::g17::availability::Availability>,
    #[pin]
    contexts: Mutex<Contexts>,
    /// Serializes binds, steals and releases: a steal holds two VMs' locks.
    /// Order: binder, GPUVM, VM binding, context allocator, provider transport.
    #[pin]
    binder: Mutex<()>,
}

/// A VM's logical execution users (queues) and, while it has published work
/// or until another VM needs it, its hardware identity.
pub(super) struct Binding {
    bound: Option<(u8, u64)>,
    users: usize,
    states: Arc<WorkStates>,
    /// Lock-free copy of `bound` for descriptor encoding (see [`encode`]).
    cell: Arc<AtomicU64>,
    /// Commands of this VM present on a physical queue, from their push at
    /// publication until their exact retirement removes them (or the queue
    /// settles them otherwise). Counted independently of command pins: an
    /// identity is never taken from a VM while either is nonzero.
    activity: Arc<AtomicU32>,
}

pub(super) struct Execution {
    pub(super) states: Arc<WorkStates>,
    cell: Arc<AtomicU64>,
    activity: Arc<AtomicU32>,
    released: AtomicBool,
    owner: Arc<UatState>,
}

/// Generation in bits 63:6 (below 2^58), identity in bits 5:0; zero while
/// unbound. Writers hold the VM binding lock; a pinned command reads a value
/// that cannot change until it retires.
fn encode(bound: Option<(u8, u64)>) -> u64 {
    bound.map_or(0, |(id, generation)| (generation << 6) | u64::from(id & 63))
}

fn decode(value: u64) -> Option<(u8, u64)> {
    (value != 0).then(|| ((value & 63) as u8, value >> 6))
}

impl UatState {
    /// A queue's logical lease on its VM. Native binds a hardware identity
    /// per submission (an identity allocation in every prepare), so
    /// creating a queue never consumes one.
    pub(super) fn execution(self: &Arc<Self>, vm: &Vm) -> Result<Arc<ExecutionContext>> {
        let (states, cell, activity) = {
            let table_owner = vm.inner.exec_lock(None, false)?;
            let table::PageTable::Sptm(table) = &table_owner.page_table else {
                return Err(EINVAL);
            };
            table.ensure_certain()?;
            let mut binding = vm.binding.lock();
            match &mut binding.sptm {
                Some(current) => {
                    current.users = current.users.checked_add(1).ok_or(EOVERFLOW)?;
                    (current.states.clone(), current.cell.clone(), current.activity.clone())
                }
                None => {
                    let states = Arc::new(WorkStates::new()?, GFP_KERNEL)?;
                    let cell = Arc::new(AtomicU64::new(0), GFP_KERNEL)?;
                    let activity = Arc::new(AtomicU32::new(0), GFP_KERNEL)?;
                    binding.sptm = Some(Binding { bound: None, users: 1, states: states.clone(),
                        cell: cell.clone(), activity: activity.clone() });
                    (states, cell, activity)
                }
            }
        };
        match Arc::new(ExecutionContext {
            root: ExecutionRoot::Sptm(Execution {
                states, cell, activity, owner: self.clone(), released: AtomicBool::new(true),
            }),
            vm: vm.clone(),
        }, GFP_KERNEL) {
            Ok(execution) => {
                if let ExecutionRoot::Sptm(root) = &execution.root {
                    root.released.store(false, Ordering::Release);
                }
                Ok(execution)
            },
            Err(error) => {
                self.drop_user(vm);
                Err(error.into())
            }
        }
    }

    fn pin_bound(&self, vm: &Vm) -> Result<Bound> {
        let binding = vm.binding.lock();
        let Some(current) = binding.sptm.as_ref().filter(|current| current.users != 0) else {
            return Err(EIO);
        };
        let Some((id, generation)) = current.bound else { return Ok(Bound::Unbound); };
        let index = usize::from(id);
        let mut contexts = self.contexts.lock();
        if contexts.generations[index] != generation {
            return Err(EIO);
        }

        let pins = &mut contexts.pins[index];
        *pins = pins.checked_add(1).ok_or(EOVERFLOW)?;
        Ok(Bound::Pinned(ContextPin { id, generation }))
    }

    /// A publication starts or stops waiting for an identity.


    /// Pin the VM's identity for one published command. An unbound VM takes
    /// the lowest free identity, else steals the lowest one whose VM has no
    /// unretired command: no pin and no command on any physical queue.
    /// EBUSY: every identity names work in flight, as native prepare()
    /// returning kIOReturnNoResources; retry after work retires.
    pub(super) fn pin(self: &Arc<Self>, vm: &Vm) -> Result<ContextPin> {
        match self.pin_bound(vm)? {
            Bound::Pinned(pin) => return Ok(pin),

            Bound::Unbound => {}
        }
        let _binder = self.binder.lock();
        for _ in 0..4 {
            match self.pin_bound(vm)? {
                Bound::Pinned(pin) => return Ok(pin),

                Bound::Unbound => {}
            }
            let (id, victim) = {
                let mut contexts = self.contexts.lock();
                let free = !contexts.used & EXECUTION_CONTEXT_MASK;
                if free != 0 {
                    let id = free.trailing_zeros() as u8;
                    contexts.used |= 1u64 << id;
                    (id, None)
                } else {
                    let mut unpinned_active = None;
                    let candidate = (0..UAT_NUM_CTX).find(|&id| {
                        if EXECUTION_CONTEXT_MASK & (1u64 << id) == 0
                            || contexts.pins[id] != 0 || contexts.owners[id].is_none() {
                            return false;
                        }
                        let active = contexts.activity[id].as_ref()
                            .map_or(0, |activity| activity.load(Ordering::Acquire));
                        if active != 0 {
                            unpinned_active.get_or_insert((id, active));
                        }
                        active == 0
                    });
                    if let Some((id, active)) = unpinned_active {
                        contexts.unpinned_active += 1;
                        let n = contexts.unpinned_active;
                        if n <= 8 || n.is_power_of_two() {
                            pr_warn!("asahi: SPTM context {} has no pin but {} command(s) of VM {} on physical queues; not taken (occurrence {})\n",
                                id, active, contexts.owners[id].as_ref().map_or(0, |vm| vm.id()), n);
                        }
                    }
                    let Some(id) = candidate else { return Err(EBUSY); };
                    (id as u8, contexts.owners[id].clone())
                }
            };
            if let Some(victim) = victim {
                match self.evict(&victim, id) {
                    Ok(()) => {}
                    Err(error) if error == EAGAIN => continue,
                    Err(error) => return Err(error),
                }
            }
            return self.bind(vm, id);
        }
        Err(EBUSY)
    }

    /// Withdraw an idle VM's identity (native: registerContextID(-1) on the
    /// victim). The victim's queues bind again when they next publish.
    fn evict(&self, victim: &Vm, id: u8) -> Result {
        let mut owner = victim.inner.exec_lock(None, false)?;
        let table::PageTable::Sptm(table) = &mut owner.page_table else { return Err(EINVAL); };
        let mut binding = victim.binding.lock();
        let Some(current) = binding.sptm.as_mut() else { return Err(EAGAIN); };
        let Some((bound, generation)) = current.bound.filter(|(bound, _)| *bound == id) else {
            return Err(EAGAIN);
        };
        {
            let contexts = self.contexts.lock();
            if contexts.pins[usize::from(id)] != 0
                || contexts.generations[usize::from(id)] != generation
                || current.activity.load(Ordering::Acquire) != 0 {
                return Err(EAGAIN);
            }
        }
        // A failed removal keeps the identity with the poisoned victim.
        if let Err(error) = table.unbind() {
            pr_warn!("asahi: SPTM context {} could not be taken from VM {}: {:?}\n", id, victim.id(), error);
            sptm_transport::note_private("steal-failed", table.private_handle(), u64::from(id));
            return Err(error);
        }
        current.bound = None;
        current.cell.store(0, Ordering::Release);
        let mut contexts = self.contexts.lock();
        // Never drop a VM reference under the allocator lock.
        let previous = contexts.owners[usize::from(id)].take();
        let activity = contexts.activity[usize::from(id)].take();
        contexts.steals += 1;
        let steals = contexts.steals;
        drop(contexts);
        drop(previous);
        drop(activity);
        if steals <= 8 || steals.is_power_of_two() {
            pr_info!("asahi: SPTM context {} taken from idle VM {} (steal {})\n", bound, victim.id(), steals);
        }
        sptm_transport::note_private("steal", table.private_handle(), u64::from(id));
        if sptm_transport::space_trace() {
            pr_info!("asahi: SPTM identity {} generation {} taken from VM {}\n", id, generation, victim.id());
        }
        Ok(())
    }

    /// Bind the reserved identity `id` to `vm` and pin it for the caller.
    fn bind(&self, vm: &Vm, id: u8) -> Result<ContextPin> {
        let index = usize::from(id);
        let unreserve = |state: &Self| { state.contexts.lock().used &= !(1u64 << id); };
        let mut owner = match vm.inner.exec_lock(None, false) {
            Ok(owner) => owner,
            Err(error) => { unreserve(self); return Err(error); }
        };
        let table::PageTable::Sptm(table) = &mut owner.page_table else {
            unreserve(self);
            return Err(EINVAL);
        };
        if let Err(error) = table.ensure_certain() {
            unreserve(self);
            return Err(error);
        }
        let mut binding = vm.binding.lock();
        let Some(current) = binding.sptm.as_mut().filter(|current| current.users != 0
            && current.bound.is_none()) else {
            unreserve(self);
            return Err(EIO);
        };
        let generation = {
            let mut contexts = self.contexts.lock();
            // M5 publishes all 58 generation bits in its native render token.
            let Some(generation) = contexts.generations[index].checked_add(1)
                .filter(|value| *value < (1u64 << 58)) else {
                contexts.used &= !(1u64 << id);
                return Err(EOVERFLOW);
            };
            contexts.generations[index] = generation;
            generation
        };
        // A failed bind may have installed the context. Keep the identity
        // quarantined; the poisoned table retains the mapping backing.
        table.bind(id)?;
        current.bound = Some((id, generation));
        current.cell.store(encode(current.bound), Ordering::Release);
        let owner = vm.clone();
        let activity = current.activity.clone();
        let mut contexts = self.contexts.lock();
        let previous = contexts.owners[index].replace(owner);
        let previous_activity = contexts.activity[index].replace(activity);
        contexts.pins[index] = 1;
        drop(contexts);
        drop(previous);
        drop(previous_activity);
        if sptm_transport::space_trace() {
            pr_info!("asahi: SPTM identity {} generation {} bound to VM {}\n", id, generation, vm.id());
        }
        Ok(ContextPin { id, generation })
    }

    /// The command retired exactly. A pin of an earlier binding of the
    /// identity (released with its VM's last queue) is ignored.
    pub(super) fn unpin(&self, pin: ContextPin) {
        let mut contexts = self.contexts.lock();
        let index = usize::from(pin.id);
        if index < UAT_NUM_CTX && contexts.generations[index] == pin.generation
            && contexts.pins[index] != 0 {
            contexts.pins[index] -= 1;
            let available = contexts.pins[index] == 0;
            drop(contexts);
            if available { self.available.notify(); }
        }
    }

    /// Drop one logical user; the last one returns the VM's identity. The
    /// caller proved scheduler release and exact retirement of every queue,
    /// so pins the VM still holds (commands that ended without an exact
    /// retirement witness) are cleared with it, as the release did before.
    fn drop_user(&self, vm: &Vm) {
        let _binder = self.binder.lock();
        let Ok(mut owner) = vm.inner.exec_lock(None, false) else {
            if let Some(status) = vm.status() { status.record(EIO); }
            sptm_transport::note_private("release-lock-failed", None, 0);
            return;
        };
        let mut binding = vm.binding.lock();
        let Some(current) = binding.sptm.as_mut().filter(|current| current.users != 0) else {
            if let Some(status) = vm.status() { status.record(EIO); }
            sptm_transport::note_private("release-mismatch", None, 0);
            return;
        };
        current.users -= 1;
        if current.users != 0 { return; }
        let Some((id, generation)) = current.bound else {
            binding.sptm = None;
            return;
        };
        // Closing the last logical queue is not a retirement witness. Keep
        // an identity with pins or physical activity bound to its original
        // VM. It becomes reclaimable only when those witnesses return.
        {
            let contexts = self.contexts.lock();
            let index = usize::from(id);
            let pins = contexts.pins[index];
            let active = current.activity.load(Ordering::Acquire);
            if contexts.generations[index] != generation
                || !contexts.owners[index].as_ref().is_some_and(|owner| owner.id() == vm.id())
                || pins != 0 || active != 0
            {
                pr_warn!("asahi: retaining closed VM {} identity {} generation {}: pins={} active={}\n",
                    vm.id(), id, generation, pins, active);
                return;
            }
        }
        let table::PageTable::Sptm(table) = &mut owner.page_table else { return; };
        if table.unbind().is_err() {
            // Keep the binding and identity quarantined. Never recycle an
            // identity merely because every host queue reported an error.
            sptm_transport::note_private("unbind-failed", table.private_handle(), u64::from(id));
            return;
        }
        sptm_transport::note_private("unbind", table.private_handle(), u64::from(id));
        current.cell.store(0, Ordering::Release);
        binding.sptm = None;
        let mut contexts = self.contexts.lock();
        let index = usize::from(id);
        let mut previous = None;
        let mut activity = None;
        if contexts.generations[index] == generation {
            contexts.used &= !(1u64 << id);
            previous = contexts.owners[index].take();
            activity = contexts.activity[index].take();
            contexts.pins[index] = 0;
        }
        drop(contexts);
        drop(binding);
        drop(owner);
        drop(_binder);
        drop(previous);
        drop(activity);
        self.available.notify();
    }
}

impl Execution {
    /// The logical lease is live (queues may publish). Its identity may be
    /// absent until the next publication pins one.
    pub(super) fn is_current(&self, vm: &Vm) -> bool {
        if self.released.load(Ordering::Acquire) { return false; }
        vm.binding.lock().sptm.as_ref().is_some_and(|current| current.users != 0)
    }

    /// Lock-free: descriptor encoders read it on every publication.
    pub(super) fn bound(&self) -> Option<(u8, u64)> { decode(self.cell.load(Ordering::Acquire)) }

    pub(super) fn pin(&self, vm: &Vm) -> Result<ContextPin> {
        if self.released.load(Ordering::Acquire) { return Err(EIO); }
        self.owner.pin(vm)
    }

    pub(super) fn unpin(&self, pin: ContextPin) { self.owner.unpin(pin) }

    pub(super) fn availability(&self) -> &Arc<crate::g17::availability::Availability> {
        &self.owner.available
    }



    /// A command of this VM entered (`true`) or left a physical queue.
    pub(super) fn note_activity(&self, entered: bool) {
        if entered {
            self.activity.fetch_add(1, Ordering::AcqRel);
        } else {
            if self.activity.fetch_sub(1, Ordering::AcqRel) == 1 {
                self.owner.available.notify();
            }
        }
    }

    pub(super) fn release(&self, vm: &Vm) {
        if self.released.swap(true, Ordering::AcqRel) { return; }
        self.owner.drop_user(vm);
    }
}

impl Vm {
    pub(crate) fn sync_sptm_context(&self) -> Result {
        let mut owner = self.inner.exec_lock(None, false)?;
        let table::PageTable::Sptm(table) = &mut owner.page_table else { return Err(EINVAL); };
        table.sync_context().map(|_| ())
    }

    fn new_sptm(
        dev: &driver::AsahiDevice, inner: Arc<UatInner>, state: &UatState,
        geometry: UatGeometry, kernel_range: Range<u64>, global: bool, id: u64,
    ) -> Result<Self> {
        let status = Arc::pin_init(crate::g17::status::VmStatus::new(), GFP_KERNEL)?;
        let table = if global {
            sptm_pgtable::PageTable::new_global(&state.transport, status.clone())
        } else {
            sptm_pgtable::PageTable::new(&state.transport, status.clone())?
        };
        sptm_transport::note_private("vm-create", table.private_handle(), id);
        Self::new_with_table(dev, inner, kernel_range, geometry,
            table::PageTable::Sptm(table), global, Some(status), id)
    }
}

/// CPU-only selftest lease: no command is ever made visible to firmware.
/// Keeping the token beside its owner also drains partial setup on every error.
struct SelftestExecution {
    execution: Arc<ExecutionContext>,
    pin: Option<ContextPin>,
}

impl SelftestExecution {
    fn new(execution: Arc<ExecutionContext>) -> Result<Self> {
        let mut lease = Self { execution, pin: None };
        lease.pin = Some(lease.execution.pin()?);
        Ok(lease)
    }

    fn retire(&mut self) {
        if let Some(pin) = self.pin.take() {
            self.execution.unpin(pin);
        }
        self.execution.release();
    }
}

impl Drop for SelftestExecution {
    fn drop(&mut self) { self.retire(); }
}

impl Uat {
    /// Firmware owns context zero for the lifetime of this session.
    pub(crate) fn bind_sptm_kernel_context(&self) -> Result {
        let mut owner = self.kernel_lower_vm.inner.exec_lock(None, false)?;
        let table::PageTable::Sptm(table) = &mut owner.page_table else { return Err(EINVAL); };
        table.bind(0)
    }

    /// Explicit bring-up diagnostic. It exercises the same GPUVM mappings and
    /// execution leases as submission without publishing a firmware command.
    pub(crate) fn sptm_selftest(dev: &driver::AsahiDevice,
        pdev: &platform::Device<device::Bound>) -> Result
    {
        let uat = Self::new_sptm(dev, pdev, UatGeometry { ias: 42, oas: 42 })?;
        let object = gem::new_kernel_object(dev, 4 * UAT_PGSZ)?;
        let global = uat.kernel_vm().map_at(sptm::GLOBAL_RANGE.start + 0x100_0000,
            4 * UAT_PGSZ, object.gem.clone(), PROT_GPU_FW_SHARED_RW, false)?;
        global.translate_offset(UAT_PGSZ + 37)?;
        drop(global);
        for prot in [PROT_FW_MMIO_RW, PROT_FW_MMIO_RO] {
            // Firmware interrupt-register window, mapped but never accessed.
            let mapping = uat.kernel_vm().map_io(sptm::GLOBAL_RANGE.start + 0x100_0000,
                0x2_8041_4000, UAT_PGSZ, prot)?;
            drop(mapping);
            if uat.kernel_vm().status().ok_or(EIO)?.get() != 0 { return Err(EIO); }
        }
        for iteration in 0..32 {
            let vm = uat.new_vm(100 + iteration, 0x300_0000..0x400_0000)?.with_status()?;
            let first = vm.map_at(0x100_0000, 4 * UAT_PGSZ,
                object.gem.clone(), PROT_GPU_SHARED_RW, false)?;
            let mut lease_a = SelftestExecution::new(uat.new_execution_context(&vm)?)?;
            let mut lease_b = SelftestExecution::new(uat.new_execution_context(&vm)?)?;
            let (a, b) = (lease_a.execution.clone(), lease_b.execution.clone());
            let (pin_a, pin_b) = (lease_a.pin.ok_or(EIO)?, lease_b.pin.ok_or(EIO)?);
            if pin_a != pin_b || a.id() != b.id() || !a.is_current() || !b.is_current()
                || !Arc::ptr_eq(&a.work_states()?, &b.work_states()?) {
                return Err(EIO);
            }
            let second = vm.map_at(0x200_0000, 4 * UAT_PGSZ,
                object.gem.clone(), PROT_GPU_SHARED_RO, false)?;
            if first.translate_offset(37)? != second.translate_offset(37)? {
                return Err(EIO);
            }
            uat.flush_vm_contexts(&vm)?;
            // Retire each pin before releasing its logical owner.
            lease_a.retire();
            if a.is_current() || !b.is_current() { return Err(EIO); }
            drop(second);
            lease_b.retire();
            if b.is_current() || uat.vm_context_mask(&vm)? != 0 { return Err(EIO); }
            let c = uat.new_execution_context(&vm)?;
            if !c.is_current() { return Err(EIO); }
            c.release();
            drop((a, b, c, first));
            vm.bo_deferred_cleanup();
            if vm.status().ok_or(EIO)?.get() != 0 { return Err(EIO); }
            drop(vm);
            dev_info!(dev.as_ref(), "M5 SPTM runtime cycle {}/32 retired\n", iteration + 1);
        }
        let mut parallel = KVec::new();
        let mut mask = 0;
        for index in 0..8 {
            let vm = uat.new_vm(200 + index, 0x300_0000..0x400_0000)?.with_status()?;
            let lease = SelftestExecution::new(uat.new_execution_context(&vm)?)?;
            let bit = 1u64 << lease.pin.ok_or(EIO)?.id;
            if mask & bit != 0 { return Err(EIO); }
            mask |= bit;
            parallel.push(lease, GFP_KERNEL)?;
        }
        drop(parallel);
        let UatBackend::Sptm(state) = &uat.inner.backend else { return Err(EIO); };
        let contexts = state.contexts.lock();
        if contexts.used & EXECUTION_CONTEXT_MASK != 0
            || contexts.pins.iter().any(|pins| *pins != 0) {
            return Err(EIO);
        }
        drop(contexts);
        dev_info!(dev.as_ref(), "M5 SPTM runtime: 32 VM reuse cycles, 8 independent bindings, global RAM/MMIO mappings retired\n");
        Ok(())
    }

    pub(super) fn new_sptm_vm(&self, state: &UatState, id: u64,
        kernel_range: Range<u64>, global: bool) -> Result<Vm>
    {
        Vm::new_sptm(&self.dev, self.inner.clone(), state, self.geometry,
            kernel_range, global, id)
    }

    /// Construct only provider-owned roots. No handoff/TTBAT memory or direct
    /// page-table root is allocated, mapped, zeroed or published on this path.
    pub(crate) fn new_sptm(dev: &driver::AsahiDevice,
        pdev: &platform::Device<device::Bound>, geometry: UatGeometry) -> Result<Self>
    {
        if geometry.ias != 42 || geometry.kernel_range() != sptm::GLOBAL_RANGE {
            return Err(EINVAL);
        }
        let transport = Arc::new(sptm_transport::Transport::new(pdev)?, GFP_KERNEL)?;
        let state = Arc::pin_init(try_pin_init!(UatState {
            transport,
            available: crate::g17::availability::Availability::new()?,
            contexts <- new_mutex!(Contexts {
                used: !EXECUTION_CONTEXT_MASK, generations: [0; UAT_NUM_CTX],
                pins: [0; UAT_NUM_CTX], owners: [const { None }; UAT_NUM_CTX],
                activity: [const { None }; UAT_NUM_CTX], steals: 0, unpinned_active: 0,
            }, "SPTM execution identities"),
            binder <- new_mutex!((), "SPTM identity binder"),
        }), GFP_KERNEL)?;
        let inner = Arc::new(UatInner {
            firmware: UatFirmware::Sptm, backend: UatBackend::Sptm(state.clone()),
        }, GFP_KERNEL)?;
        let kernel_vm = Vm::new_sptm(dev, inner.clone(), &state, geometry,
            geometry.kernel_range(), true, 0)?;
        let kernel_lower_vm = Vm::new_sptm(dev, inner.clone(), &state, geometry,
            geometry.user_range(), false, 1)?;
        Ok(Self {
            dev: dev.into(), geometry, inner, kernel_vm, kernel_lower_vm,
            slots: slotalloc::SlotAllocator::new(UAT_USER_CTX as u32, (),
                |_inner, _slot| Some(SlotInner()), c_str!("Uat::SlotAllocator"),
                static_lock_class!(), static_lock_class!())?,
        })
    }
}

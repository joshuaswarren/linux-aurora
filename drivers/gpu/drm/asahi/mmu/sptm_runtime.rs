// SPDX-License-Identifier: GPL-2.0-only

//! Runtime ownership for provider-managed roots. Queues share a VM's one
//! hardware binding, but retain independent scheduler and retirement owners.

use super::*;
use crate::g17::context::identity::WorkStates;
use kernel::platform;

struct Contexts {
    used: u64,
    generations: [u64; UAT_NUM_CTX],
}

#[pin_data]
pub(super) struct UatState {
    transport: Arc<sptm_transport::Transport>,
    #[pin]
    contexts: Mutex<Contexts>,
}

pub(super) struct Binding {
    id: u8,
    generation: u64,
    users: usize,
    states: Arc<WorkStates>,
}

pub(super) struct Execution {
    pub(super) id: u8,
    pub(super) generation: u64,
    pub(super) states: Arc<WorkStates>,
    released: AtomicBool,
    owner: Arc<UatState>,
}

impl UatState {
    fn free(&self, id: u8) {
        self.contexts.lock().used &= !(1u64 << id);
    }

    pub(super) fn execution(self: &Arc<Self>, vm: &Vm) -> Result<Arc<ExecutionContext>> {
        // Lock order: GPUVM, VM binding, context allocator, provider transport.
        let mut table_owner = vm.inner.exec_lock(None, false)?;
        let table::PageTable::Sptm(table) = &mut table_owner.page_table else {
            return Err(EINVAL);
        };
        table.ensure_certain()?;
        let mut binding = vm.binding.lock();
        if let Some(current) = &mut binding.sptm {
            let users = current.users.checked_add(1).ok_or(EOVERFLOW)?;
            let execution = Arc::new(ExecutionContext {
                root: ExecutionRoot::Sptm(Execution {
                    id: current.id, generation: current.generation,
                    states: current.states.clone(), owner: self.clone(),
                    released: AtomicBool::new(true),
                }),
                vm: vm.clone(),
            }, GFP_KERNEL)?;
            current.users = users;
            if let ExecutionRoot::Sptm(root) = &execution.root {
                root.released.store(false, Ordering::Release);
            }
            return Ok(execution);
        }
        let states = Arc::new(WorkStates::new(), GFP_KERNEL)?;
        let (id, generation) = {
            let mut contexts = self.contexts.lock();
            let free = !contexts.used & EXECUTION_CONTEXT_MASK;
            if free == 0 { return Err(ENOSPC); }
            let id = free.trailing_zeros() as usize;
            // M5 publishes all 58 generation bits in its native render token.
            // Keep the full counter across ID reuse; the legacy byte field is
            // narrowed only at its own descriptor boundary.
            let generation = contexts.generations[id].checked_add(1)
                .filter(|value| *value < (1u64 << 58)).ok_or(EOVERFLOW)?;
            contexts.used |= 1u64 << id;
            contexts.generations[id] = generation;
            (id as u8, contexts.generations[id])
        };
        // Host allocations precede context publication. The inactive token's
        // destructor must not attempt an unbind while these locks are held.
        let execution = Arc::new(ExecutionContext {
            root: ExecutionRoot::Sptm(Execution {
                id, generation, states: states.clone(), owner: self.clone(),
                released: AtomicBool::new(true),
            }),
            vm: vm.clone(),
        }, GFP_KERNEL).inspect_err(|_| self.free(id))?;
        // A failed bind may have installed the context. Keep its allocator bit
        // quarantined; the poisoned table retains the mapping backing.
        table.bind(id)?;
        binding.sptm = Some(Binding { id, generation, users: 1, states });
        if let ExecutionRoot::Sptm(root) = &execution.root {
            root.released.store(false, Ordering::Release);
        }
        Ok(execution)
    }
}

impl Execution {
    pub(super) fn is_current(&self, vm: &Vm) -> bool {
        if self.released.load(Ordering::Acquire) { return false; }
        let binding = vm.binding.lock();
        binding.sptm.as_ref().is_some_and(|current|
            current.users != 0 && current.id == self.id
            && current.generation == self.generation
            && Arc::ptr_eq(&current.states, &self.states))
    }

    pub(super) fn release(&self, vm: &Vm) {
        if self.released.swap(true, Ordering::AcqRel) { return; }
        let Ok(mut owner) = vm.inner.exec_lock(None, false) else {
            if let Some(status) = vm.status() { status.record(EIO); }
            return;
        };
        let mut binding = vm.binding.lock();
        let Some(current) = binding.sptm.as_mut() else { return; };
        if current.id != self.id || current.generation != self.generation
            || !Arc::ptr_eq(&current.states, &self.states) || current.users == 0 {
            if let Some(status) = vm.status() { status.record(EIO); }
            return;
        }
        current.users -= 1;
        if current.users != 0 { return; }
        let table::PageTable::Sptm(table) = &mut owner.page_table else { return; };
        if table.unbind().is_err() {
            // Keep the binding and context ID quarantined. Never recycle an
            // identity merely because every host queue reported an error.
            return;
        }
        binding.sptm = None;
        self.owner.free(self.id);
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
        Self::new_with_table(dev, inner, kernel_range, geometry,
            table::PageTable::Sptm(table), global, Some(status), id)
    }
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
            let a = uat.new_execution_context(&vm)?;
            let b = uat.new_execution_context(&vm)?;
            if a.id() != b.id() || !a.is_current() || !b.is_current()
                || !Arc::ptr_eq(&a.work_states()?, &b.work_states()?) {
                return Err(EIO);
            }
            let second = vm.map_at(0x200_0000, 4 * UAT_PGSZ,
                object.gem.clone(), PROT_GPU_SHARED_RO, false)?;
            if first.translate_offset(37)? != second.translate_offset(37)? {
                return Err(EIO);
            }
            uat.flush_vm_contexts(&vm)?;
            a.release();
            if a.is_current() || !b.is_current() { return Err(EIO); }
            drop(second);
            b.release();
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
            let execution = uat.new_execution_context(&vm)?;
            let bit = 1u64 << execution.id();
            if mask & bit != 0 { return Err(EIO); }
            mask |= bit;
            parallel.push(execution, GFP_KERNEL)?;
        }
        for execution in &parallel { execution.release(); }
        drop(parallel);
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
        let state = Arc::pin_init(pin_init!(UatState {
            transport,
            contexts <- new_mutex!(Contexts {
                used: !EXECUTION_CONTEXT_MASK, generations: [0; UAT_NUM_CTX],
            }, "SPTM execution identities"),
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

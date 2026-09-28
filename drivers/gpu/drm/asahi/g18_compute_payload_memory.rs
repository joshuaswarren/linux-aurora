// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Retained private G18 compute fixture in its own, unpublished process VM.
//! Context-root publication, command aliases, device policy and the queue kick
//! are separate requirements. Loading this object never submits work.

#![allow(dead_code)]

use super::MappedObject;
use crate::{driver::AsahiDevice, g18_compute::*, g18_compute_payload as payload,
    g18_compute_state::{G18ComputeState, G18_COMPUTE_STATE_BYTES}, mmu};
use core::sync::atomic::{fence, Ordering};
use kernel::{firmware::Firmware, prelude::*};

pub(crate) struct G18ComputePayloadMemory {
    // Mappings and their GEM storage are released before the VM root owner.
    pages: KVec<MappedObject>,
    vm: mmu::Vm,
    state: [u8; G18_COMPUTE_STATE_BYTES],
    output: usize,
}

impl G18ComputePayloadMemory {
    pub(crate) fn load(dev: &AsahiDevice, uat: &mmu::Uat) -> Result<Self> {
        if !uat.is_t8152_geometry() || mmu::UAT_PGSZ != payload::PAGE_BYTES {
            return Err(ENODEV);
        }
        let firmware = Firmware::request(c"asahi/g18-26a428-compute-add.bin", dev.as_ref())?;
        let reference = payload::ComputePayload::parse(firmware.data()).map_err(|_| EINVAL)?;
        let vm = uat.new_t8152_compute_fixture_vm()?;
        let mut pages = KVec::new();
        let mut output = None;
        for index in 0..reference.page_count() {
            let input = reference.page(index).ok_or(EINVAL)?;
            let mut page = MappedObject::new_wc_at(dev, &vm, input.address,
                payload::PAGE_BYTES, mmu::PROT_GPU_SHARED_RW)?;
            page.with_bytes_mut(|bytes| {
                bytes.copy_from_slice(input.bytes);
                if input.address == payload::OUTPUT {
                    // Defense in depth: never inherit a capture's answer.
                    for word in bytes.chunks_exact_mut(4) {
                        word.copy_from_slice(&payload::POISON.to_le_bytes());
                    }
                }
                Ok(())
            })?;
            vm.translate_iova(input.address)?;
            vm.translate_iova(input.address + payload::PAGE_BYTES as u64 - 1)?;
            if input.address == payload::OUTPUT { output = Some(index); }
            pages.push(page, GFP_KERNEL)?;
        }
        let state = reference.state_bytes().try_into().map_err(|_| EINVAL)?;
        let mut owner = Self { pages, vm, state, output: output.ok_or(EINVAL)? };
        let initial = owner.output_snapshot()?;
        if initial.checked != 4096 || initial.untouched != 4096 || initial.correct != 0 {
            return Err(EIO);
        }
        fence(Ordering::SeqCst);
        dev_info!(dev.as_ref(),
            "G18 compute payload: {} pages retained, all 4096 outputs poisoned; private VM roots unpublished\n",
            owner.pages.len());
        Ok(owner)
    }

    pub(crate) fn vm(&self) -> &mmu::Vm { &self.vm }

    pub(crate) fn apply_state(&self, c: &mut G18ComputeCommandInputs, r: &mut G18ComputeRegisterInputs) {
        G18ComputeState::parse(&self.state).unwrap().apply(c, r);
    }

    /// Call only while the GPU is quiescent or after an independently verified
    /// completion fence. This read does not establish completion itself.
    pub(crate) fn output_snapshot(&mut self) -> Result<payload::ComputeResult> {
        fence(Ordering::SeqCst);
        self.pages[self.output].with_bytes_mut(|bytes|
            payload::ComputeResult::inspect(bytes).ok_or(EINVAL))
    }
}

// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Linux-owned pages for the bounded M5 native-program graphics experiment.
//!
//! Allocation is separate from root publication and queue submission. A future
//! job owner must keep this entire graph alive until both firmware CPUs stop,
//! including after faults. Captured physical backing is never installed.

use super::MappedObject;
use crate::{driver::AsahiDevice, m5_graphics_bundle as bundle, mmu};
use kernel::{firmware::Firmware, prelude::*};

pub(super) struct M5GraphicsMemory {
    // Retract any published roots before dropping their mapping owners.
    context: mmu::M5ExecutionContext,
    pages: KVec<MappedObject>,
    output_first: usize,
}

impl M5GraphicsMemory {
    pub(super) fn load(dev: &AsahiDevice, uat: &mmu::Uat) -> Result<Self> {
        let firmware = Firmware::request(c"asahi/m5-25g76-graphics.bin", dev.as_ref())?;
        let reference = bundle::GraphicsBundle::parse(firmware.data()).map_err(|_| EINVAL)?;
        Self::new(dev, uat, &reference)
    }

    fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        reference: &bundle::GraphicsBundle<'_>,
    ) -> Result<Self> {
        let context = uat.new_t8142_execution_context()?;
        let mut pages = KVec::new();
        let mut output_first = None;
        for index in 0..reference.page_count() {
            let input = reference.page(index).ok_or(EINVAL)?;
            let protection = if input.writable {
                mmu::PROT_GPU_SHARED_RW
            } else {
                mmu::PROT_GPU_SHARED_RO
            };
            let mut page = MappedObject::new_wc_at(
                dev,
                context.vm(),
                input.address,
                bundle::PAGE_BYTES,
                protection,
            )?;
            page.with_bytes_mut(|bytes| {
                bytes.copy_from_slice(input.bytes);
                Ok(())
            })?;
            context.vm().translate_iova(input.address)?;
            if input.address == bundle::PIXEL_ADDRESS {
                output_first = Some(index);
            }
            pages.push(page, GFP_KERNEL)?;
        }
        Ok(Self {
            context,
            pages,
            output_first: output_first.ok_or(EINVAL)?,
        })
    }

    /// Job construction can add its own objects before root publication.
    pub(super) fn vm(&self) -> &mmu::Vm {
        self.context.vm()
    }

    pub(super) fn roots(&self) -> [u64; 2] {
        self.context.roots()
    }

    /// Alias existing owned user pages into the firmware VM. PM page lists
    /// and scene counters must share backing with their user GPU views.
    /// The returned mappings belong to the eventual retained job owner.
    pub(super) fn alias_kernel_pages(
        &mut self,
        uat: &mmu::Uat,
        user_address: u64,
        kernel_address: u64,
        bytes: usize,
    ) -> Result<KVec<mmu::KernelMapping>> {
        if self.context.is_current() {
            return Err(EBUSY);
        }
        if bytes == 0
            || bytes > bundle::MAX_PAGES * bundle::PAGE_BYTES
            || bytes % bundle::PAGE_BYTES != 0
            || (user_address | kernel_address) & (bundle::PAGE_BYTES as u64 - 1) != 0
        {
            return Err(EINVAL);
        }
        user_address.checked_add(bytes as u64).ok_or(EOVERFLOW)?;
        kernel_address.checked_add(bytes as u64).ok_or(EOVERFLOW)?;
        let mut indices = KVec::new();
        for offset in (0..bytes).step_by(bundle::PAGE_BYTES) {
            let address = user_address + offset as u64;
            let index = self
                .pages
                .iter()
                .position(|page| page.iova() == address)
                .ok_or(EFAULT)?;
            indices.push(index, GFP_KERNEL)?;
        }
        let mut aliases = KVec::new();
        for (ordinal, index) in indices.iter().enumerate() {
            let high = kernel_address + (ordinal * bundle::PAGE_BYTES) as u64;
            let alias =
                self.pages[*index].map_alias_at(uat.kernel_vm(), high, mmu::PROT_FW_SHARED_RW)?;
            if uat.kernel_vm().translate_iova(high)?
                != self
                    .context
                    .vm()
                    .translate_iova(self.pages[*index].iova())?
            {
                return Err(EFAULT);
            }
            aliases.push(alias, GFP_KERNEL)?;
        }
        Ok(aliases)
    }

    /// Read a word from a retained reference page while preparing its job.
    pub(super) fn read_word(&mut self, address: u64) -> Result<u32> {
        if address & 3 != 0 {
            return Err(EINVAL);
        }
        let base = address & !(bundle::PAGE_BYTES as u64 - 1);
        let page = self
            .pages
            .iter_mut()
            .find(|page| page.iova() == base)
            .ok_or(EFAULT)?;
        page.read_u32(address)
    }

    /// Call only after the complete TA/3D resource graph is prepared.
    pub(super) fn publish(&mut self) -> Result {
        self.context.publish()
    }

    pub(super) fn is_current(&self) -> bool {
        self.context.is_current()
    }

    /// Read only after a stopped/unsubmitted graph or confirmed completion.
    /// Pixel success alone does not prove queue retirement or presentation.
    pub(super) fn result(&mut self) -> Result<bundle::TriangleResult> {
        let mut result = bundle::TriangleResult::default();
        for page_index in 0..bundle::PIXEL_PAGES {
            self.pages[self.output_first + page_index].with_bytes_mut(|bytes| {
                for word in 0..bundle::PAGE_BYTES / 4 {
                    // SAFETY: word stays within this aligned, retained WC
                    // mapping. The GPU may have updated these output bytes.
                    let value = unsafe {
                        core::ptr::read_volatile(bytes.as_ptr().add(word * 4).cast::<u32>())
                    };
                    result.add(
                        page_index * (bundle::PAGE_BYTES / 4) + word,
                        value.to_le_bytes(),
                    );
                }
                Ok(())
            })?;
        }
        Ok(result)
    }
}

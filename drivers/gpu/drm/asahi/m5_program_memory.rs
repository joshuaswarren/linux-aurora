// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Owned M5 diagnostic program pages; no work publication.

use super::MappedObject;
use crate::{driver::AsahiDevice, m5_compute_program as program, mmu};
use kernel::prelude::*;

pub(super) struct M5ProgramMemory {
    // Retract the roots before releasing any page mappings.
    context: mmu::M5ExecutionContext,
    pages: KVec<MappedObject>,
}

impl M5ProgramMemory {
    pub(super) fn new(dev: &AsahiDevice, uat: &mmu::Uat) -> Result<Self> {
        let mut context = uat.new_t8142_execution_context()?;
        let mut pages = KVec::new();
        for (address, kind) in [
            (program::SHADER_PAGE, program::PageKind::Shader),
            (program::PIPELINE_PAGE, program::PageKind::Pipeline),
            (program::STREAM_PAGE, program::PageKind::Stream),
            (program::PREEMPT_PAGE, program::PageKind::Preempt),
            (program::BUFFER_PAGE, program::PageKind::Buffers),
            (program::ROBUSTNESS_PAGE, program::PageKind::Robustness),
        ] {
            let mut page = MappedObject::new_wc_at(dev, context.vm(), address,
                program::PAGE_BYTES, mmu::PROT_GPU_SHARED_RW)?;
            page.with_bytes_mut(|out| program::initialize_page(kind, out).map_err(|_| EINVAL))?;
            // Validate each installed leaf before publishing the root pair.
            context.vm().translate_iova(address)?;
            pages.push(page, GFP_KERNEL)?;
        }
        context.publish()?;
        Ok(Self { context, pages })
    }

    pub(super) fn validate_unsubmitted(&mut self) -> Result<[u64; 2]> {
        if !self.context.is_current() || self.pages.len() != 6 {
            return Err(EFAULT);
        }
        let result = self.pages[4].with_bytes_mut(|out| {
            program::check_result(out).map_err(|_| EINVAL)
        })?;
        if result.inputs_changed != 0 || result.outputs_wrong != program::ELEMENTS
            || result.outputs_untouched != program::ELEMENTS {
            return Err(EIO);
        }
        Ok(self.context.roots())
    }
}

// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Pins for the two explicit M5 startup diagnostics. Firmware owns these
//! before diagnostic construction can publish anything. Its existing startup
//! failure retention keeps them on every later error; only stopped firmware
//! may release them. They are not scope guards around a constructor or run().

use super::context::Context;
use crate::mmu::ContextPin;
use kernel::{prelude::*, sync::Arc};

struct Pin {
    context: Arc<Context>,
    identity: ContextPin,
    qos: u8,
}

pub(super) struct Pins {
    // One compute and one render diagnostic; neither can run twice per boot.
    slots: [Option<Pin>; 2],
}

impl Pins {
    pub(super) fn new() -> Self {
        Self { slots: [None, None] }
    }

    /// Retain both claims before any diagnostic queue or pool publication.
    /// Failure here is still unpublished, so the partial claim can roll back.
    pub(super) fn acquire(&mut self, context: &Arc<Context>) -> Result {
        let slot = self.slots.iter_mut().find(|slot| slot.is_none()).ok_or(ENOSPC)?;
        let identity = context.pin()?;
        let qos = match context.pin_qos() {
            Ok(qos) => qos,
            Err(error) => {
                context.unpin(identity);
                return Err(error);
            }
        };
        *slot = Some(Pin { context: context.clone(), identity, qos });
        Ok(())
    }

    /// Caller has stopped both processors and disabled the firmware bridge,
    /// at the same lifetime boundary that permits Firmware fields to drop.
    /// Constructor/run errors must retain this ledger instead of calling here.
    pub(super) fn release_after_stop(&mut self) {
        for slot in &mut self.slots {
            if let Some(pin) = slot.take() {
                pin.context.unpin_qos(pin.qos);
                pin.context.unpin(pin.identity);
            }
        }
    }
}

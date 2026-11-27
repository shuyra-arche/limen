//! Native supervisor doorbells using the admitted SBI firmware service.

use crate::metal::{
    platform::{
        description::devicetree::NodeId,
        firmware::sbi::{HartService, IpiService},
    },
    processor::hart::{
        local::try_current_hart,
        riscv64::description::{RiscvCpu, RiscvCpus},
    },
};

use super::supervisor::Reception;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Scope,
    Busy,
    Unconfirmed,
    Firmware(isize),
}

/// One discovered destination in the firmware CPU scope.
pub struct Target<'tree> {
    scope: NodeId<'tree>,
    hart: usize,
}

impl<'tree> Target<'tree> {
    pub fn describe(cpu: &RiscvCpu<'_, 'tree>) -> Option<Self> {
        cpu.enabled().then_some(())?;
        Some(Self {
            scope: cpu.node().parent_id()?,
            hart: usize::try_from(cpu.id()).ok()?,
        })
    }
}

/// One source Hart's SBI doorbell endpoint. SBI admission covers only the
/// sending operation. The destination owns the delivery and any work notice.
pub struct Sender<'tree> {
    scope: NodeId<'tree>,
    source: usize,
    harts: HartService,
    interrupts: IpiService,
}

impl<'tree> Sender<'tree> {
    pub fn bind(
        cpus: &RiscvCpus<'tree>,
        source: usize,
        harts: HartService,
        interrupts: IpiService,
    ) -> Result<Self, Error> {
        if try_current_hart().is_none_or(|hart| hart.get() != source) {
            return Err(Error::Scope);
        }
        if !cpus
            .cpu(source as u64)
            .ok()
            .flatten()
            .is_some_and(|cpu| cpu.enabled())
        {
            return Err(Error::Scope);
        }
        Ok(Self {
            scope: cpus.node().id(),
            source,
            harts,
            interrupts,
        })
    }

    /// Deliver an idempotent notification to the exact checked destination.
    /// A firmware failure leaves delivery unknown. SBI success reports sending,
    /// while the receiver owns the separately observed pending bit. Publish any
    /// work to the receiver's synchronized mailbox before calling this method.
    pub fn notify(&self, target: &Target<'tree>) -> Result<(), Error> {
        if try_current_hart().is_none_or(|hart| hart.get() != self.source)
            || target.scope != self.scope
            || target.hart == self.source
            || !self.harts.started(target.hart)
        {
            return Err(Error::Scope);
        }
        self.interrupts.send(target.hart).map_err(Error::Firmware)
    }
}

/// The original Hart's supervisor software-interrupt line.
#[must_use]
pub struct Receiver(Reception);

impl Receiver {
    /// # Safety
    /// The executing Hart grants this receiver exclusive SSIE/SSIP control.
    /// The actual IRQ entry is ready, ordinary interrupts are masked, and all
    /// older notifications and concurrent configuration are excluded.
    pub unsafe fn admit(hart: usize) -> Result<Self, Error> {
        let hart = u64::try_from(hart).map_err(|_| Error::Scope)?;
        unsafe { Reception::admit_software(hart) }
            .map(Self)
            .map_err(|_| Error::Scope)
    }

    fn current(&self) -> Result<(), Error> {
        self.0.valid().then_some(()).ok_or(Error::Scope)
    }

    pub fn open(&mut self) -> Result<(), Error> {
        self.current()?;
        self.0.set(true);
        self.0.enabled().then_some(()).ok_or(Error::Unconfirmed)
    }

    pub fn pending(&self) -> Result<bool, Error> {
        self.current()?;
        Ok(self.0.pending())
    }

    /// Clear a pending doorbell. Multiple sends can share the same bit.
    /// A concurrent notification can reassert it before observation and must
    /// be handled via the original mailbox protocol.
    pub fn acknowledge(&mut self) -> Result<(), Error> {
        if !self.pending()? {
            return Err(Error::Busy);
        }
        self.0.clear_software();
        if self.0.pending() {
            Err(Error::Unconfirmed)
        } else {
            Ok(())
        }
    }

    /// # Safety
    /// Exclude new sends and retain any already-published mailbox work.
    /// Confirm that the receiver is shut without discarding a pending IRQ.
    pub unsafe fn shut(&mut self) -> Result<(), Error> {
        if self.pending()? {
            return Err(Error::Busy);
        }
        self.0.set(false);
        if self.0.enabled() {
            Err(Error::Unconfirmed)
        } else {
            Ok(())
        }
    }
}

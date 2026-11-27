//! Native supervisor doorbells using the admitted SBI firmware service.

use crate::metal::{
    interrupt::ipi::{Issuance, NotifyHart, Sealed},
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
    unconfirmed: Option<(usize, isize)>,
}

/// A borrow of the sender bound to the original ready remote receiver.
pub struct IpiRoute<'a, 'tree> {
    owner: &'a mut Sender<'tree>,
    target: &'a Target<'tree>,
}

impl Sealed for IpiRoute<'_, '_> {}

impl NotifyHart for IpiRoute<'_, '_> {
    type Error = Error;

    fn notify(&mut self) -> Result<Issuance<Error>, Error> {
        match self.owner.notify(self.target) {
            Ok(()) => Ok(Issuance::Confirmed),
            Err(error @ Error::Firmware(_)) => Ok(Issuance::Unconfirmed(error)),
            Err(error) => Err(error),
        }
    }

    fn pending(&mut self) -> Result<Option<Issuance<Error>>, Error> {
        self.owner.check_source()?;
        match self.owner.unconfirmed {
            Some((hart, error)) if hart == self.target.hart => {
                Ok(Some(Issuance::Unconfirmed(Error::Firmware(error))))
            }
            Some(_) => Err(Error::Busy),
            None => Ok(None),
        }
    }
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
            unconfirmed: None,
        })
    }

    /// # Safety
    /// The destination is the original ready receiver for the caller's
    /// published work. Keep that exact Hart participation and its receiving
    /// line live through pending issues and independent reception.
    pub unsafe fn bind_ipi<'a>(
        &'a mut self,
        target: &'a Target<'tree>,
    ) -> Result<IpiRoute<'a, 'tree>, Error> {
        self.check_target(target)?;
        if self
            .unconfirmed
            .is_some_and(|(hart, _)| hart != target.hart)
        {
            return Err(Error::Busy);
        }
        Ok(IpiRoute {
            owner: self,
            target,
        })
    }

    fn check_source(&self) -> Result<(), Error> {
        if try_current_hart().is_some_and(|hart| hart.get() == self.source) {
            Ok(())
        } else {
            Err(Error::Scope)
        }
    }

    fn check_target(&self, target: &Target<'tree>) -> Result<(), Error> {
        self.check_source()?;
        if target.scope == self.scope
            && target.hart != self.source
            && self.harts.started(target.hart)
        {
            Ok(())
        } else {
            Err(Error::Scope)
        }
    }

    /// Deliver an idempotent notification to the exact checked destination.
    /// A firmware failure leaves delivery unknown. SBI success reports sending,
    /// while the receiver owns the separately observed pending bit. Publish any
    /// work to the receiver's synchronized mailbox before calling this method.
    pub fn notify(&mut self, target: &Target<'tree>) -> Result<(), Error> {
        self.check_target(target)?;
        if self.unconfirmed.is_some() {
            return Err(Error::Busy);
        }
        if let Err(error) = self.interrupts.send(target.hart) {
            self.unconfirmed = Some((target.hart, error));
            return Err(Error::Firmware(error));
        }
        Ok(())
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

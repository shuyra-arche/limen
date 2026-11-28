//! Physical fixed-vector notifications through the original xAPIC ICR.

use super::{
    Failure, LocalApic,
    registers::{Destination, Icr, LEVEL_ASSERT},
};
use crate::metal::{
    interrupt::treaty::{Issuance, NotifyProcessor, ipi::Sealed},
    platform::description::acpi::Madt,
};
use core::sync::atomic::{Ordering, fence};
use limen_treaty::model::PhysicalAddress;

/// An enabled physical destination and fixed interrupt vector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpiTarget {
    controller: PhysicalAddress,
    destination: Destination,
    vector: u8,
}

impl IpiTarget {
    pub fn describe(madt: Madt<'_>, processor: usize, vector: u8) -> Option<Self> {
        let destination = Destination::new(processor)?;
        (madt.enabled_local_apic(destination.0) && vector >= 32).then_some(())?;
        Some(Self {
            controller: madt.local_controller_address()?,
            destination,
            vector,
        })
    }

    pub fn processor(self) -> u8 {
        self.destination.0
    }

    pub fn vector(self) -> u8 {
        self.vector
    }

    #[cfg(all(limen_target, target_arch = "x86_64"))]
    pub(super) fn controller(self) -> PhysicalAddress {
        self.controller
    }

    fn command(self) -> u32 {
        // Fixed, physical destination, edge trigger and no shorthand.
        LEVEL_ASSERT | u32::from(self.vector)
    }
}

/// The exact original fixed-vector issue in the local ICR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpiIssue {
    source: usize,
    target: IpiTarget,
    round: u64,
}

impl IpiIssue {
    pub fn target(self) -> IpiTarget {
        self.target
    }
}

/// A borrow of the sender bound to the original ready remote receiver.
pub struct IpiRoute<'a, 'map> {
    owner: &'a mut LocalApic<'map>,
    target: IpiTarget,
}

impl Sealed for IpiRoute<'_, '_> {}

impl NotifyProcessor for IpiRoute<'_, '_> {
    type Error = Failure;
    type Issue = IpiIssue;

    fn notify(&mut self) -> Result<Issuance<IpiIssue, Failure>, Failure> {
        self.owner.notify(self.target).map(Issuance::Pending)
    }

    fn pending(&self) -> Result<Option<IpiIssue>, Failure> {
        self.owner.reception_context()?;
        match self.owner.icr.notification() {
            Some(original) if original.target() == self.target => Ok(Some(original)),
            Some(_) => Err(Failure::Busy),
            None => Ok(None),
        }
    }

    fn confirm(&mut self, original: IpiIssue) -> Result<Issuance<IpiIssue, Failure>, Failure> {
        if self.pending()? != Some(original) {
            return Err(Failure::Busy);
        }
        if !self.owner.wait_icr() {
            return Ok(Issuance::Pending(original));
        }
        self.owner.icr = Icr::Idle;
        Ok(Issuance::Confirmed(original))
    }
}

impl<'map> LocalApic<'map> {
    /// # Safety
    /// If the target passes the native scope checks, it must identify the
    /// original live remote processor, vector, gate and reception route. Retain
    /// that path through pending issues and independent reception.
    pub unsafe fn bind_ipi(&mut self, target: IpiTarget) -> Result<IpiRoute<'_, 'map>, Failure> {
        self.check_ipi_target(target)?;
        if self.icr != Icr::Idle && self.icr.notification().map(IpiIssue::target) != Some(target) {
            return Err(Failure::Busy);
        }
        Ok(IpiRoute {
            owner: self,
            target,
        })
    }

    fn check_ipi_target(&mut self, target: IpiTarget) -> Result<(), Failure> {
        self.reception_context()?;
        if self.base.physical.start() == target.controller
            && self.processor != usize::from(target.processor())
        {
            Ok(())
        } else {
            Err(Failure::Scope)
        }
    }

    /// Reserve the original issue before triggering the physical ICR write.
    fn notify(&mut self, target: IpiTarget) -> Result<IpiIssue, Failure> {
        self.check_ipi_target(target)?;
        if self.icr != Icr::Idle || !self.wait_icr() {
            return Err(Failure::Busy);
        }
        let round = self.ipi_round.checked_add(1).ok_or(Failure::Busy)?;
        let issue = IpiIssue {
            source: self.processor,
            target,
            round,
        };
        self.ipi_round = round;
        self.icr = Icr::Notifying(issue);
        // The ICR write is the remote notification. Order prior normal
        // mailbox publication before the write to this UC device.
        fence(Ordering::SeqCst);
        self.write_icr(target.destination, target.command());
        Ok(issue)
    }
}

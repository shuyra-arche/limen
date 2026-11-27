//! Physical fixed-vector notifications through the original xAPIC ICR.

use super::{Destination, Failure, LEVEL_ASSERT, LocalApic, State};
use crate::metal::{
    interrupt::ipi::{Issuance, NotifyHart, Sealed},
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
    pub fn describe(madt: Madt<'_>, hart: usize, vector: u8) -> Option<Self> {
        let destination = Destination::new(hart)?;
        (madt.enabled_local_apic(destination.0) && vector >= 32).then_some(())?;
        Some(Self {
            controller: madt.local_controller_address()?,
            destination,
            vector,
        })
    }

    pub fn hart(self) -> u8 {
        self.destination.0
    }

    pub fn vector(self) -> u8 {
        self.vector
    }

    fn command(self) -> u32 {
        // Fixed, physical destination, edge trigger and no shorthand.
        LEVEL_ASSERT | u32::from(self.vector)
    }
}

/// An observation of the one original send. A dropped borrow leaves its
/// exact target retained in the Local APIC, without issuing another write.
#[must_use]
pub struct IpiSend<'a, 'map> {
    owner: &'a mut LocalApic<'map>,
    target: IpiTarget,
}

impl IpiSend<'_, '_> {
    pub fn target(&self) -> IpiTarget {
        self.target
    }

    /// Settle this local ICR operation when the delivery-pending bit clears.
    /// Only the receiving Hart can establish actual vector receipt and EOI.
    pub fn observe(&mut self) -> Result<(), Failure> {
        self.owner.reception_context()?;
        if self.owner.ipi != Some(self.target) {
            return Err(Failure::Busy);
        }
        if !self.owner.idle() {
            return Err(Failure::Unconfirmed);
        }
        self.owner.ipi = None;
        Ok(())
    }
}

/// A borrow of the sender bound to the original ready remote receiver.
pub struct IpiRoute<'a, 'map> {
    owner: &'a mut LocalApic<'map>,
    target: IpiTarget,
}

impl Sealed for IpiRoute<'_, '_> {}

impl NotifyHart for IpiRoute<'_, '_> {
    type Error = Failure;

    fn notify(&mut self) -> Result<Issuance<Failure>, Failure> {
        let _issued = unsafe { self.owner.notify(self.target) }?;
        Ok(Issuance::Pending)
    }

    fn pending(&mut self) -> Result<Option<Issuance<Failure>>, Failure> {
        self.owner.reception_context()?;
        let Some(mut original) = self.owner.pending_ipi() else {
            return Ok(None);
        };
        if original.target() != self.target {
            return Err(Failure::Busy);
        }
        match original.observe() {
            Ok(()) => Ok(Some(Issuance::Confirmed)),
            Err(Failure::Unconfirmed) => Ok(Some(Issuance::Pending)),
            Err(error) => Err(error),
        }
    }
}

impl<'map> LocalApic<'map> {
    /// # Safety
    /// The target is the live remote Hart whose original vector, installed
    /// gate and reception route the caller has qualified. Retain the exact
    /// remote path through pending issues and independent reception.
    pub unsafe fn bind_ipi(&mut self, target: IpiTarget) -> Result<IpiRoute<'_, 'map>, Failure> {
        self.check_ipi_target(target)?;
        if self.ipi.is_some_and(|original| original != target) {
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
            && self.hart != usize::from(target.hart())
        {
            Ok(())
        } else {
            Err(Failure::Scope)
        }
    }

    /// Request a physical fixed interrupt. Retain the requested target
    /// before the triggering ICR write and across uncertain observation.
    ///
    /// # Safety
    /// Publish any work before sending and keep its ownership independently.
    /// The target belongs to the same live xAPIC domain and already holds
    /// the original vector lease, installed gate and receiving authority.
    /// Keep those alive until the target independently settles the effect.
    pub unsafe fn notify(&mut self, target: IpiTarget) -> Result<IpiSend<'_, 'map>, Failure> {
        self.check_ipi_target(target)?;
        if self.state != State::Idle || self.ipi.is_some() || !self.idle() {
            return Err(Failure::Busy);
        }
        self.ipi = Some(target);
        // The ICR write is the remote notification. Order prior normal
        // mailbox publication before the write to this UC device.
        fence(Ordering::SeqCst);
        self.write_icr(target.destination, target.command());
        Ok(IpiSend {
            owner: self,
            target,
        })
    }

    /// Resume observation of a sent command without sending it again.
    pub fn pending_ipi(&mut self) -> Option<IpiSend<'_, 'map>> {
        let target = self.ipi?;
        Some(IpiSend {
            owner: self,
            target,
        })
    }
}

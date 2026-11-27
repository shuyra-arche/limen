//! Native SBI notification issuance and supervisor software reception.

use crate::metal::{
    interrupt::treaty::{Issuance, NotifyProcessor, ipi::Sealed},
    platform::firmware::treaty::sbi::{HartService, IpiService},
    processor::{description::riscv64::cpu::RiscvCpus, treaty::try_current_processor},
};

use super::super::supervisor::Reception;
use super::{Error, Record, SendState, Source, Target};

/// One source Hart's SBI doorbell endpoint. SBI admission covers only the
/// sending operation. The destination owns the delivery and any work notice.
pub struct Sender<'state, 'tree> {
    source: Source<'tree>,
    state: &'state mut SendState<'tree>,
    harts: HartService,
    interrupts: IpiService,
}

/// A borrow of the sender bound to the original ready remote receiver.
pub struct IpiRoute<'a, 'state, 'tree> {
    owner: &'a mut Sender<'state, 'tree>,
    target: &'a Target<'tree>,
}

/// The exact original firmware CPU scope, sender, destination and issue round.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpiIssue<'tree> {
    source: Source<'tree>,
    hart: usize,
    round: u64,
}

impl<'tree> IpiIssue<'tree> {
    fn new(source: Source<'tree>, record: Record) -> Self {
        Self {
            source,
            hart: record.hart,
            round: record.round,
        }
    }
}

impl Sealed for IpiRoute<'_, '_, '_> {}

impl<'tree> NotifyProcessor for IpiRoute<'_, '_, 'tree> {
    type Error = Error;
    type Issue = IpiIssue<'tree>;

    fn notify(&mut self) -> Result<Issuance<Self::Issue, Error>, Error> {
        self.owner.notify(self.target)
    }

    fn pending(&self) -> Result<Option<Self::Issue>, Error> {
        self.owner.check_source()?;
        Ok(self
            .owner
            .state
            .pending(self.target.hart)?
            .map(|record| IpiIssue::new(self.owner.source, record)))
    }

    fn confirm(&mut self, original: Self::Issue) -> Result<Issuance<Self::Issue, Error>, Error> {
        self.owner.check_source()?;
        let record = self
            .owner
            .state
            .pending(self.target.hart)?
            .ok_or(Error::Busy)?;
        if IpiIssue::new(self.owner.source, record) != original {
            return Err(Error::Busy);
        }
        // The SBI IPI service has no readback for a failed ecall. In particular
        // a receiver rechecking published work cannot attribute coalesced
        // interrupts to this individual issue.
        Ok(match record.error {
            Some(error) => Issuance::Unconfirmed(original, Error::Firmware(error)),
            None => Issuance::Pending(original),
        })
    }
}

impl<'state, 'tree> Sender<'state, 'tree> {
    /// # Safety
    /// The actual sending Hart owner supplies its one resident record.
    /// Preserve it across sender drops and re-admissions, including after
    /// uncertain effects. Only a never-used native source starts fresh.
    /// Exclude any independent issuer for the same source.
    pub unsafe fn bind(
        cpus: &RiscvCpus<'tree>,
        hart: usize,
        state: &'state mut SendState<'tree>,
        harts: HartService,
        interrupts: IpiService,
    ) -> Result<Self, Error> {
        if try_current_processor().is_none_or(|current| current.get() != hart) {
            return Err(Error::Scope);
        }
        if !cpus
            .cpu(hart as u64)
            .ok()
            .flatten()
            .is_some_and(|cpu| cpu.enabled())
        {
            return Err(Error::Scope);
        }
        let source = Source {
            scope: cpus.node().id(),
            hart,
        };
        state.admit(source)?;
        Ok(Self {
            source,
            state,
            harts,
            interrupts,
        })
    }

    /// # Safety
    /// If the destination passes the native scope checks, it must be the
    /// original ready receiver for the caller's published work. Keep that
    /// Hart participation and line live through any pending issue and receipt.
    pub unsafe fn bind_ipi<'a>(
        &'a mut self,
        target: &'a Target<'tree>,
    ) -> Result<IpiRoute<'a, 'state, 'tree>, Error> {
        self.check_scope(target)?;
        if self.state.pending(target.hart)?.is_none() && !self.harts.started(target.hart) {
            return Err(Error::Scope);
        }
        Ok(IpiRoute {
            owner: self,
            target,
        })
    }

    fn check_source(&self) -> Result<(), Error> {
        if try_current_processor().is_some_and(|hart| hart.get() == self.source.hart) {
            Ok(())
        } else {
            Err(Error::Scope)
        }
    }

    fn check_scope(&self, target: &Target<'tree>) -> Result<(), Error> {
        self.check_source()?;
        if target.scope == self.source.scope && target.hart != self.source.hart {
            Ok(())
        } else {
            Err(Error::Scope)
        }
    }

    /// Deliver an idempotent notification to the exact checked destination.
    /// A firmware failure leaves delivery unknown. SBI success reports sending,
    /// while the receiver owns the separately observed pending bit. Publish any
    /// work to the receiver's synchronized mailbox before calling this method.
    fn notify(
        &mut self,
        target: &Target<'tree>,
    ) -> Result<Issuance<IpiIssue<'tree>, Error>, Error> {
        self.check_scope(target)?;
        if !self.harts.started(target.hart) {
            return Err(Error::Scope);
        }
        let record = self.state.begin(target.hart)?;
        let original = IpiIssue::new(self.source, record);
        // Firmware may raise the interrupt through device output. Order
        // ordinary work publication before those native effects.
        unsafe { core::arch::asm!("fence rw, iorw", options(nostack)) };
        let result = self.interrupts.send(target.hart);
        self.state.finish(result);
        Ok(match result {
            Ok(()) => Issuance::Confirmed(original),
            Err(error) => Issuance::Unconfirmed(original, Error::Firmware(error)),
        })
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

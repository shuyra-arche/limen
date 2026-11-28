//! The semantic entry for a captured same-level interrupt.

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use crate::metal::processor::treaty::Irq;
use crate::metal::processor::treaty::{Capture, Entry, Handler};
#[cfg(target_arch = "riscv64")]
use crate::metal::processor::treaty::{SoftwareInterrupt, TimerInterrupt};

/// Permanent, same-image binding of the actual receiver owner and its matching
/// handling functions. The capture borrows only the current machine layer.
/// Any obligation surviving the callback belongs to the original owner.
pub struct Binding<Owner> {
    owner: Owner,
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    irq: for<'frame> fn(&Owner, Irq<'frame>),
    #[cfg(target_arch = "riscv64")]
    software: Option<for<'frame> fn(&Owner, SoftwareInterrupt<'frame>)>,
    #[cfg(target_arch = "riscv64")]
    timer: Option<for<'frame> fn(&Owner, TimerInterrupt<'frame>)>,
}

impl<Owner: Sync + 'static> Binding<Owner> {
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    pub const fn new(owner: Owner, irq: for<'frame> fn(&Owner, Irq<'frame>)) -> Self {
        Self { owner, irq }
    }

    /// The supervisor software and timer sources have independent receivers.
    #[cfg(target_arch = "riscv64")]
    pub const fn new(
        owner: Owner,
        software: Option<for<'frame> fn(&Owner, SoftwareInterrupt<'frame>)>,
        timer: Option<for<'frame> fn(&Owner, TimerInterrupt<'frame>)>,
    ) -> Self {
        Self {
            owner,
            software,
            timer,
        }
    }

    /// The owner may also be used through its ordinary access contract.
    pub const fn owner(&self) -> &Owner {
        &self.owner
    }

    pub const fn entry(&'static self) -> Entry {
        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        {
            Entry::new(self)
        }
        #[cfg(target_arch = "riscv64")]
        {
            Entry::new(self, self.software.is_some(), self.timer.is_some())
        }
    }
}

impl<Owner: Sync + 'static> Handler for Binding<Owner> {
    fn handle(&self, capture: Capture<'_>) {
        match capture {
            #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
            Capture::Interrupt(interrupt) => (self.irq)(&self.owner, interrupt),
            #[cfg(target_arch = "riscv64")]
            Capture::Software(interrupt) => {
                let receive = self.software.expect("admitted software receiver");
                receive(&self.owner, interrupt);
            }
            #[cfg(target_arch = "riscv64")]
            Capture::Timer(interrupt) => {
                let receive = self.timer.expect("admitted timer receiver");
                receive(&self.owner, interrupt);
            }
        }
    }
}

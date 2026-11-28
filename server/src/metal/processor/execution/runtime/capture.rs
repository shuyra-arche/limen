#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use super::Irq;
#[cfg(target_arch = "riscv64")]
use super::{SoftwareInterrupt, TimerInterrupt};

/// A captured native event borrowing this layer of the admitted local entry.
pub(crate) enum Capture<'frame> {
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    Interrupt(Irq<'frame>),
    #[cfg(target_arch = "riscv64")]
    Software(SoftwareInterrupt<'frame>),
    #[cfg(target_arch = "riscv64")]
    Timer(TimerInterrupt<'frame>),
}

pub(crate) trait Handler: Sync {
    fn handle(&self, capture: Capture<'_>);
}

/// A resident handler and the native sources admitted by its fixed binding.
///
/// The machine owner keeps the frame and decides the legal return. Completion
/// of the handler proves only that control reached the end of this callback.
pub struct Entry {
    handler: &'static dyn Handler,
    #[cfg(target_arch = "riscv64")]
    pub(super) software: bool,
    #[cfg(target_arch = "riscv64")]
    pub(super) timer: bool,
}

impl Entry {
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    pub(crate) const fn new(handler: &'static dyn Handler) -> Self {
        Self { handler }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) const fn new(handler: &'static dyn Handler, software: bool, timer: bool) -> Self {
        Self {
            handler,
            software,
            timer,
        }
    }

    pub(super) fn handle(&self, capture: Capture<'_>) {
        self.handler.handle(capture);
    }
}

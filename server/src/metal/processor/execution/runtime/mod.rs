//! Installed same-level runtime entries, with an ISA-owned save shape.

mod capture;
pub use capture::Entry;
pub(crate) use capture::{Capture, Handler};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::{Irq, RuntimeTrap};
#[cfg(target_arch = "riscv64")]
pub(in crate::metal::processor) use riscv64::Suspension;
#[cfg(target_arch = "riscv64")]
pub use riscv64::{RuntimeTrap, SoftwareInterrupt, TimerInterrupt};
#[cfg(target_arch = "x86_64")]
pub use x86_64::{Irq, RuntimeTrap};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallError {
    Context,
    Occupied,
}

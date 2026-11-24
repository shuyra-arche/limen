//! Trusted handoff on the boot CPU. The loader keeps the image, stacks and
//! handoff inputs reserved while they are in use.
//!
//! - RV64: OpenSBI enters `_start`. Metal masks interrupts, establishes the
//!   boot and trap stacks and clears BSS. The entry checks supervisor state,
//!   SBI console/reset support and the DTB extent before delivering `Boot`.
//! - x86_64: after ExitBootServices, the loader calls `enter` in 64-bit ring
//!   0 with paging active. Metal masks interrupts, switches stacks and loads
//!   a fail-stop IDT before calling the continuation with its input word.
//! - AArch64: the loader calls `enter` in non-secure EL1. Metal masks DAIF,
//!   selects SP_EL1 and installs the fail-stop vectors before forwarding the
//!   input word to the continuation.
//!
//! All three expose the boot CPU stack reservation through [`stack_range`].

#[cfg(target_arch = "riscv64")]
mod handoff;
#[cfg(target_arch = "riscv64")]
mod tree;

#[cfg(target_arch = "riscv64")]
pub use crate::metal::riscv::sbi::{shutdown, write};
#[cfg(target_arch = "riscv64")]
pub use handoff::Boot;

#[cfg(target_arch = "aarch64")]
pub use crate::metal::aarch64::enter;
#[cfg(target_arch = "x86_64")]
pub use crate::metal::x86_64::enter;

#[cfg(target_arch = "riscv64")]
pub use crate::metal::riscv::boot::stack_range;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub use crate::metal::stack::stack_range;

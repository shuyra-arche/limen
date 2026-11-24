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

pub use crate::metal::console::{Console, console, shutdown, write};
pub use crate::metal::stack::stack_range;

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub use crate::metal::platform::enter;
#[cfg(target_arch = "riscv64")]
pub use handoff::Boot;

use crate::metal::platform;
use core::convert::Infallible;

/// Every firmware entry reaches this gate after Metal installs the boot stack
/// and exception entry. A platform continuation may reject its loader input.
pub(crate) fn start(deliver: impl FnOnce() -> Result<Infallible, &'static str>) -> ! {
    let services = platform::services();
    if !crate::metal::stack::active()
        || !platform::established()
        || (platform::REQUIRES_DIAGNOSTICS
            && (services.output.is_none() || services.reset.is_none()))
    {
        services.reject(format_args!(
            "FAIL Limen boot: invalid CPU or firmware handoff\n"
        ));
    }
    let Err(reason) = deliver();
    services.reject(format_args!("FAIL Limen boot: {reason}\n"))
}

// The loader ABI has already been translated by the time this function runs.
// x86 calls it from Rust; AArch64 branches here without using the loader stack.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(crate) extern "C" fn loaded(input: usize, next: platform::Continuation) -> ! {
    start(|| next(input))
}

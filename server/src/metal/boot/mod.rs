//! Common startup admission and launch with the selected machine entry.

pub(crate) mod console;
#[cfg(limen_target)]
pub(crate) mod major;
#[cfg(limen_target)]
pub(crate) mod minor;
#[cfg(limen_target)]
pub(crate) mod services;
#[cfg(limen_target)]
pub(in crate::metal) mod trap;
#[cfg(limen_target)]
pub(crate) use trap::stack_range as trap_stack_range;

#[cfg(all(limen_target, target_arch = "riscv64"))]
#[path = "riscv/mod.rs"]
mod backend;
#[cfg(all(limen_target, target_arch = "x86_64"))]
#[path = "x86_64/mod.rs"]
mod backend;
#[cfg(all(limen_target, target_arch = "aarch64"))]
#[path = "aarch64/mod.rs"]
mod backend;

#[cfg(all(limen_target, target_arch = "riscv64"))]
use super::firmware::sbi as firmware;
#[cfg(all(limen_target, any(target_arch = "x86_64", target_arch = "aarch64")))]
use super::firmware::uefi as firmware;

#[cfg(limen_target)]
pub use backend::Startup;
#[cfg(all(limen_target, any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(crate) use backend::enter_major;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub(crate) use backend::image;
#[cfg(limen_target)]
pub(crate) use backend::{
    DeliveryError, MinorContext, established, minor_context, park, wake_minor,
};

/// Major and minor CPUs use the same ordinary startup stack budget.
#[cfg(limen_target)]
pub(in crate::metal) const STACK_BYTES: usize = 32 * 1024;

/// During bootstrap, terminal vectors switch to the diagnostic stack before
/// invoking Rust. Ordinary entry and Hart protection require the installed
/// bootstrap vectors and must exclude that terminal stack.
#[cfg(limen_target)]
pub(crate) fn ordinary_entry() -> bool {
    backend::entry_installed()
        && !trap::stack_range().contains(&crate::metal::hart::stack_pointer())
}

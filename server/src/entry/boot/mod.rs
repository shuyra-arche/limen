//! Major CPU handoff. Metal installs the kernel stack and exception entry before
//! calling the common admission gate. The trusted image receives Boot exactly once.
mod handoff;
mod tree;

pub use super::platform::{console, shutdown, write};
pub use crate::metal::console::Console;
pub use crate::metal::firmware::uefi::FirmwareMap;
pub use crate::metal::stack::stack_range;
pub use handoff::Boot;
#[cfg(not(target_arch = "riscv64"))]
pub use {crate::metal::boot::major, crate::metal::firmware::uefi::Loader};

use crate::metal::boot;

/// The firmware adapter supplies a stable DTB (OpenSBI) or Loader (UEFI).
/// All referenced storage and mappings remain reserved until the consumer is done.
pub(crate) unsafe extern "C" fn start(hart: usize, input: *const ()) -> ! {
    let services = super::platform::services();
    if !crate::metal::stack::active()
        || !boot::established()
        || (boot::REQUIRES_DIAGNOSTICS && (services.output.is_none() || services.reset.is_none()))
    {
        services.reject(format_args!(
            "FAIL Limen boot: invalid CPU or firmware handoff\n"
        ));
    }
    #[cfg(any(
        target_arch = "riscv64",
        all(target_arch = "aarch64", feature = "qemu-virt"),
        all(target_arch = "x86_64", feature = "qemu-q35")
    ))]
    if hart != boot::current_hart() {
        services.reject(format_args!(
            "FAIL Limen boot: major CPU identity mismatch\n"
        ));
    }
    let admitted = unsafe { Boot::admit(hart, input) };
    let Some(boot) =
        admitted.filter(|boot| !super::platform::REQUIRES_TREE || boot.device_tree().is_some())
    else {
        services.reject(format_args!("FAIL Limen boot: invalid firmware input\n"));
    };
    unsafe { limen_platform_ready(boot) }
}

unsafe extern "Rust" {
    fn limen_platform_ready(boot: Boot<'_>) -> !;
}

#[cfg(any(
    target_arch = "riscv64",
    all(target_arch = "x86_64", feature = "qemu-q35"),
    all(target_arch = "aarch64", feature = "qemu-virt")
))]
pub use crate::metal::minor::{Minor, StartError};

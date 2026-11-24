//! Major CPU handoff. Metal installs the kernel stack and exception entry before
//! calling the common admission gate. The trusted image receives Boot exactly once.
mod handoff;
mod tree;

pub use crate::metal::console::{Console, Services, console, shutdown, write};
pub use crate::metal::firmware::uefi::FirmwareMap;
#[cfg(not(target_arch = "riscv64"))]
pub use crate::metal::firmware::uefi::Loader;
pub use crate::metal::stack::stack_range;
pub use handoff::Boot;

/// Hand off a loaded image after successful ExitBootServices.
///
/// # Safety
/// Run once on the major CPU in ring 0 with paging (x86) or non-secure EL1 (Arm).
/// Keep the image, identity mappings, loader stack and inputs reserved across delivery.
#[cfg(not(target_arch = "riscv64"))]
pub unsafe fn major(hart: usize, loader: &Loader<'_>, services: &'static Services) -> ! {
    crate::metal::console::install(services);
    unsafe { boot::major(hart, loader) }
}

use crate::metal::boot;

/// The firmware adapter supplies a stable DTB (OpenSBI) or Loader (UEFI).
/// All referenced storage and mappings remain reserved until the consumer is done.
pub(crate) unsafe extern "C" fn start(hart: usize, input: *const ()) -> ! {
    let services = crate::metal::console::services();
    if !crate::metal::stack::active()
        || !boot::established()
        || (boot::REQUIRES_DIAGNOSTICS && (services.output.is_none() || services.reset.is_none()))
    {
        services.reject(format_args!(
            "FAIL Limen boot: invalid CPU or firmware handoff\n"
        ));
    }

    if hart != boot::current_hart() {
        services.reject(format_args!(
            "FAIL Limen boot: major CPU identity mismatch\n"
        ));
    }
    let Some(boot) = (unsafe { Boot::admit(hart, input) }) else {
        services.reject(format_args!("FAIL Limen boot: invalid firmware input\n"));
    };
    unsafe { limen_platform_ready(boot) }
}

unsafe extern "Rust" {
    fn limen_platform_ready(boot: Boot<'_>) -> !;
}

pub use crate::metal::minor::{Minor, StartError};

#[cfg(target_arch = "aarch64")]
pub use crate::metal::aarch64::firmware::psci::Conduit;
pub use crate::metal::boot::Startup;

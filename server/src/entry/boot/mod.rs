//! Major CPU handoff. Metal installs the kernel stack and exception entry before
//! calling the common admission gate. The trusted image receives Boot exactly once.
mod handoff;
use crate::metal::generic::devicetree;
use core::fmt::Write;

pub use crate::metal::generic::firmware::uefi::FirmwareMap;
#[cfg(not(target_arch = "riscv64"))]
pub use crate::metal::generic::firmware::uefi::Loader;
pub use crate::metal::generic::stack::stack_range;
pub use crate::metal::generic::{console::Console, services::Services};
pub use handoff::Boot;

/// Hand off a loaded image after successful ExitBootServices.
///
/// # Safety
/// Run once on the major CPU in ring 0 with paging (x86) or non-secure EL1 (Arm).
/// Keep the image, identity mappings, loader stack and inputs reserved across delivery.
#[cfg(not(target_arch = "riscv64"))]
pub unsafe fn major(hart: usize, loader: &Loader<'_>, services: &'static Services) -> ! {
    services.install();
    unsafe { boot::major(hart, loader) }
}

use crate::metal::boot;

/// The firmware adapter supplies a stable DTB (OpenSBI) or Loader (UEFI).
/// All referenced storage and mappings remain reserved until the consumer is done.
pub(crate) unsafe extern "C" fn start(hart: usize, input: *const ()) -> ! {
    #[cfg(target_arch = "riscv64")]
    crate::metal::generic::firmware::sbi::install();
    let services = Services::active();
    if !crate::metal::generic::stack::active() || !boot::established() {
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

/// Obtain early output when supplied by the boot environment.
pub fn console() -> Option<Console> {
    Services::active().console()
}

/// Write a best-effort diagnostic.
pub fn write(message: &str) {
    if let Some(mut console) = console() {
        let _ = console.write_str(message);
    }
}

/// Request platform termination and park if the request returns.
pub fn shutdown(success: bool) -> ! {
    Services::active().shutdown(success)
}

unsafe extern "Rust" {
    fn limen_platform_ready(boot: Boot<'_>) -> !;
}

pub use crate::metal::generic::minor::{Minor, StartError};

pub use crate::metal::boot::Startup;
#[cfg(target_arch = "aarch64")]
pub use crate::metal::generic::firmware::psci::Conduit;

//! Major CPU handoff. Metal installs the kernel stack and exception entry before
//! calling the common admission gate. The trusted image receives Boot exactly once.
mod handoff;
use crate::metal::boot::major;
use core::fmt::Write;

pub use crate::metal::boot::{Startup, console::Console, services::Services};
#[cfg(target_arch = "riscv64")]
pub use crate::metal::firmware::devicetree::memory::{DtbMemory, Physical, Reservation, Source};
pub use crate::metal::firmware::map::{FirmwareMap, FirmwareRegion, MapError};
#[cfg(target_arch = "aarch64")]
pub use crate::metal::firmware::psci::Conduit;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub use crate::metal::firmware::uefi::{Loader, major};
pub use handoff::Boot;
pub use major::stack_range;

/// The firmware adapter supplies a stable DTB (OpenSBI) or Loader (UEFI).
/// All referenced storage and mappings remain reserved until the consumer is done.
pub(crate) unsafe extern "C" fn start(hart: usize, input: *const ()) -> ! {
    let boot = unsafe { major::admit(hart, input) };
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

pub use crate::metal::boot::minor::{Minor, StartError};

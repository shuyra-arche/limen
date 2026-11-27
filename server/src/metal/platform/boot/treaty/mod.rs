//! Trusted image entry and early diagnostics.

#[cfg(limen_firmware = "uefi")]
use crate::metal::platform::firmware::uefi::Loader;
#[cfg(limen_target)]
use crate::metal::processor::treaty::Console;
#[cfg(limen_target)]
use core::fmt::Write;

/// Early output and reset operations supplied by the trusted image.
#[cfg(limen_target)]
#[derive(Clone, Copy)]
pub struct Services {
    pub output: Option<fn(u8) -> bool>,
    pub reset: Option<fn(bool)>,
}

/// Obtain early output when supplied by the boot environment.
#[cfg(limen_target)]
pub fn console() -> Option<Console> {
    Services::active().terminal().console()
}

/// Write a best-effort diagnostic.
#[cfg(limen_target)]
pub fn write(message: &str) {
    if let Some(mut console) = console() {
        let _ = console.write_str(message);
    }
}

/// Request platform termination and park if the request returns.
#[cfg(limen_target)]
pub fn shutdown(success: bool) -> ! {
    Services::active().terminal().shutdown(success)
}

/// Hand off a loaded image after successful ExitBootServices.
///
/// # Safety
/// Run once on the major CPU in ring 0 with paging (x86) or non-secure EL1 (Arm).
/// Keep the image, identity mappings, loader stack and inputs reserved across delivery.
#[cfg(limen_firmware = "uefi")]
pub unsafe fn major(hart: usize, loader: &Loader<'_>, services: &'static Services) -> ! {
    services.install();
    unsafe { super::enter_major(hart, loader) }
}

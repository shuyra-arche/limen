//! Board selection for the trusted platform image.
use crate::metal::console::Console;
use core::fmt::Write;

pub(super) const REQUIRES_TREE: bool = cfg!(any(
    target_arch = "riscv64",
    all(target_arch = "aarch64", feature = "qemu-virt")
));

#[cfg(all(target_arch = "x86_64", feature = "qemu-q35"))]
mod q35;
#[cfg(all(target_arch = "aarch64", feature = "qemu-virt"))]
mod virt;

#[cfg(target_arch = "riscv64")]
pub(super) use crate::metal::riscv::firmware::sbi::services;
#[cfg(all(target_arch = "x86_64", feature = "qemu-q35"))]
pub(super) use q35::services;
#[cfg(all(target_arch = "aarch64", feature = "qemu-virt"))]
pub(super) use virt::services;

#[cfg(not(any(
    target_arch = "riscv64",
    all(target_arch = "x86_64", feature = "qemu-q35"),
    all(target_arch = "aarch64", feature = "qemu-virt")
)))]
pub(super) fn services() -> crate::metal::console::Services {
    crate::metal::console::Services {
        output: None,
        reset: None,
    }
}

/// Obtain a writer only if this platform supplies a diagnostic device.
pub fn console() -> Option<Console> {
    services().console()
}

/// Best-effort output for trusted early boot code.
pub fn write(message: &str) {
    if let Some(mut console) = console() {
        let _ = console.write_str(message);
    }
}

/// Request platform termination and park if no reset is available or it returns.
pub fn shutdown(success: bool) -> ! {
    services().shutdown(success)
}

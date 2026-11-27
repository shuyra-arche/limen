//! Trusted image entry and early diagnostics.

pub(crate) use super::handoff::Descriptions;
#[cfg(limen_target)]
pub(crate) use super::major::Admission;

#[cfg(limen_target)]
pub(super) fn deliver(admission: Admission<'_>) -> ! {
    unsafe { limen_major_admitted(admission) }
}

#[cfg(limen_target)]
unsafe extern "Rust" {
    fn limen_major_admitted(admission: Admission<'_>) -> !;
}

#[cfg(limen_firmware = "uefi")]
use crate::metal::platform::firmware::uefi::Loader;
#[cfg(limen_target)]
use crate::metal::processor::treaty::{Console, Terminal};
#[cfg(limen_target)]
use core::fmt::Write;

#[cfg(limen_target)]
use crate::metal::processor::executor::local::{
    self, LocalStorage,
    stack::{Stack, StackExtent},
};

/// Resident materials supplied by the trusted image for its major CPU.
#[cfg(limen_target)]
pub struct MajorMaterial {
    pub(in crate::metal::platform::boot) stack: StackExtent,
    pub(in crate::metal::platform::boot) local: local::Material,
    pub(in crate::metal::platform::boot) descriptions: super::handoff::DescriptionStorage,
}

#[cfg(limen_target)]
impl MajorMaterial {
    pub const fn new<const BYTES: usize, const N: usize>(
        stack: &'static Stack<BYTES>,
        local: &'static LocalStorage<N>,
    ) -> Self {
        Self {
            stack: stack.extent(),
            local: local.material(),
            descriptions: super::handoff::DescriptionStorage::new(),
        }
    }
}

/// Define the major materials required by the RV64 image entry ABI.
/// The image selects both fixed bounds here, before firmware enters the CPU.
#[cfg(limen_firmware = "sbi")]
#[macro_export]
macro_rules! limen_riscv_major {
    (stack: $bytes:expr, nodes: $nodes:expr) => {
        #[unsafe(link_section = ".bss.limen_major_stack")]
        static LIMEN_MAJOR_STACK: $crate::metal::processor::treaty::Stack<{ $bytes }> =
            $crate::metal::processor::treaty::Stack::new();
        static LIMEN_MAJOR_LOCAL: $crate::metal::processor::treaty::LocalStorage<{ $nodes }> =
            $crate::metal::processor::treaty::LocalStorage::new(
                $crate::metal::sync::treaty::Supply::new(),
            );
        #[unsafe(no_mangle)]
        static LIMEN_MAJOR_MATERIAL: $crate::metal::platform::boot::treaty::MajorMaterial =
            $crate::metal::platform::boot::treaty::MajorMaterial::new(
                &LIMEN_MAJOR_STACK,
                &LIMEN_MAJOR_LOCAL,
            );
    };
}

/// Obtain early output when supplied by the boot environment.
#[cfg(limen_target)]
pub fn console() -> Option<Console> {
    Terminal::early().console()
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
    Terminal::early().shutdown(success)
}

/// Hand off a loaded image after successful ExitBootServices.
///
/// # Safety
/// Run once on the major CPU in ring 0 with paging (x86) or non-secure EL1 (Arm).
/// The image must exclusively supply this stack and local storage for the
/// major CPU and ensure it can hold the complete boot and platform callback.
/// Keep the image and identity mappings reserved. The loader and its ACPI
/// read function must remain valid through admission. Descriptor, DTB and
/// ACPI bytes used by admitted consumers remain independently reserved for
/// those borrows.
#[cfg(limen_firmware = "uefi")]
pub unsafe fn major(
    processor: usize,
    loader: &Loader<'_>,
    terminal: &'static Terminal,
    material: &'static MajorMaterial,
) -> ! {
    terminal.install_early();
    unsafe { super::enter_major(processor, loader, material.stack.top(), material) }
}

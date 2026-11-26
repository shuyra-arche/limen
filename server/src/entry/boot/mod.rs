//! Major CPU handoff. Metal installs the kernel stack and exception entry before
//! calling the common admission gate. The trusted image receives Boot exactly once.
mod handoff;
mod physical;
mod regions;
#[cfg(limen_target)]
use crate::metal::allocator::RegionDescription;
use crate::metal::allocator::bump;
#[cfg(limen_target)]
use crate::metal::boot::major;
#[cfg(limen_target)]
use core::fmt::Write;
#[cfg(limen_target)]
use limen_treaty::model::ByteLength;

#[cfg(limen_target)]
pub use crate::metal::boot::{Startup, console::Console, services::Services};
use crate::metal::firmware::acpi;
#[cfg(limen_target)]
use crate::metal::firmware::facts::Descriptions;
use crate::metal::firmware::{
    devicetree::{
        self,
        memory::{DtbMemory, Source},
    },
    map::{FirmwareMap, FirmwareRegion},
};
pub use handoff::Boot;
#[cfg(limen_target)]
pub use major::stack_range;
pub use physical::{Condition, Exclusion, Exclusions, Occupant, Retirement};
pub use regions::{RegionError, Regions};

#[cfg(limen_target)]
/// The firmware adapter supplies a stable DTB (OpenSBI) or Loader (UEFI).
/// All referenced storage and mappings remain reserved until the consumer is done.
pub(crate) unsafe extern "C" fn start(hart: usize, input: *const ()) -> ! {
    // The nonreturning platform callback borrows the original descriptions
    // from here. Keep a single retained copy off the nested admission frames.
    let mut descriptions = Descriptions::new();
    let facts = unsafe { major::admit(hart, input, &mut descriptions) };
    let boot = Boot::new(hart, facts.image, facts.firmware_map, &descriptions);
    if !boot.stacks_in_image() {
        Services::active().reject(format_args!(
            "FAIL Limen boot: stacks outside physical image\n"
        ));
    }
    unsafe { limen_platform_ready(boot) }
}

#[cfg(limen_target)]
impl Boot<'_> {
    /// Organize the part of the firmware RAM backed by retirement evidence.
    pub fn regions<'a>(
        &'a self,
        granule: ByteLength,
        retirement: Option<&'a Retirement>,
        storage: &'a mut [Option<RegionDescription>],
    ) -> Result<Regions<'a>, RegionError> {
        Regions::organize(self, &self.exclusions(), granule, retirement, storage)
    }

    pub fn exclusions(&self) -> Exclusions<'_> {
        self.exclusions_for(
            physical::extent(stack_range()),
            physical::extent(crate::metal::boot::trap_stack_range()),
        )
    }

    fn stacks_in_image(&self) -> bool {
        let image = self.image();
        image.covers(physical::extent(stack_range()))
            && image.covers(physical::extent(crate::metal::boot::trap_stack_range()))
    }
}

/// Obtain early output when supplied by the boot environment.
#[cfg(limen_target)]
pub fn console() -> Option<Console> {
    Services::active().console()
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
    Services::active().shutdown(success)
}

#[cfg(limen_target)]
unsafe extern "Rust" {
    fn limen_platform_ready(boot: Boot<'_>) -> !;
}

#[cfg(limen_target)]
pub use crate::metal::boot::minor::{Minor, StartError};

#[cfg(test)]
#[path = "../../../tests/entry/boot/mod.rs"]
pub(crate) mod tests;

/// Hand off a loaded image after successful ExitBootServices.
///
/// # Safety
/// Run once on the major CPU in ring 0 with paging (x86) or non-secure EL1 (Arm).
/// Keep the image, identity mappings, loader stack and inputs reserved across delivery.
#[cfg(limen_firmware = "uefi")]
pub unsafe fn major(
    hart: usize,
    loader: &crate::metal::firmware::uefi::Loader<'_>,
    services: &'static Services,
) -> ! {
    services.install();
    unsafe { crate::metal::boot::enter_major(hart, loader) }
}

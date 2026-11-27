//! Major CPU handoff. Metal installs the kernel stack and exception entry before
//! calling the common admission gate. The trusted image receives Boot exactly once.
mod handoff;
mod physical;
mod regions;
use crate::metal::memory::allocator::bootstrap::bump;
#[cfg(limen_target)]
use crate::metal::memory::allocator::treaty::RegionDescription;
#[cfg(limen_target)]
use crate::metal::platform::boot::major;
#[cfg(limen_target)]
use limen_treaty::model::ByteLength;

#[cfg(limen_target)]
use crate::metal::platform::boot::handoff::Descriptions;
use crate::metal::platform::description::treaty::acpi;
use crate::metal::platform::{
    description::treaty::devicetree::{
        self,
        memory::{DtbMemory, Source},
    },
    firmware::treaty::map::{FirmwareMap, FirmwareRegion},
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
    let admission = unsafe { major::admit(hart, input, &mut descriptions) };
    let boot = Boot::from_major(admission, &descriptions);
    if !boot.stacks_in_image() {
        boot.terminal().reject(format_args!(
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
            physical::extent(crate::metal::platform::boot::trap_stack_range()),
        )
    }

    fn stacks_in_image(&self) -> bool {
        let image = self.image();
        image.covers(physical::extent(stack_range()))
            && image.covers(physical::extent(
                crate::metal::platform::boot::trap_stack_range(),
            ))
    }
}

#[cfg(limen_target)]
unsafe extern "Rust" {
    fn limen_platform_ready(boot: Boot<'_>) -> !;
}

#[cfg(test)]
#[path = "../../../tests/entry/boot/mod.rs"]
pub(crate) mod tests;

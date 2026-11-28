//! Major CPU handoff. Consume the completed Metal admission and deliver Boot
//! once to the trusted image.
mod handoff;
mod physical;
mod regions;
#[cfg(limen_target)]
use crate::metal::memory::allocator::treaty::RegionDescription;
#[cfg(limen_target)]
use crate::metal::platform::boot::treaty::Admission;
#[cfg(limen_target)]
use limen_treaty::model::ByteLength;

use crate::metal::platform::description::treaty::acpi;
use crate::metal::platform::{
    description::treaty::devicetree::{
        self,
        memory::{DtbMemory, Source},
    },
    firmware::treaty::map::{FirmwareMap, FirmwareRegion},
};
pub use handoff::Boot;
pub use physical::{Condition, Exclusion, Exclusions, Occupant, Retirement};
pub use regions::{RegionError, Regions};

#[cfg(limen_target)]
#[unsafe(no_mangle)]
extern "Rust" fn limen_major_admitted(admission: Admission<'_>) -> ! {
    unsafe { limen_platform_ready(Boot::from_major(admission)) }
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
        let [major, trap] = self.physical_stacks();
        self.exclusions_for(major, trap)
    }
}

#[cfg(limen_target)]
unsafe extern "Rust" {
    fn limen_platform_ready(boot: Boot<'_>) -> !;
}

#[cfg(test)]
#[path = "../../../tests/entry/boot/mod.rs"]
pub(crate) mod tests;

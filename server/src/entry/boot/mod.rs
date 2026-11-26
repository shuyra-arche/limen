//! Major CPU handoff. Metal installs the kernel stack and exception entry before
//! calling the common admission gate. The trusted image receives Boot exactly once.
mod handoff;
mod physical;
mod regions;
use crate::metal::allocator::{RegionDescription, bump};
use crate::metal::boot::major;
use core::fmt::Write;
use limen_treaty::model::ByteLength;

pub use crate::metal::boot::{Startup, console::Console, services::Services};
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub use crate::metal::firmware::acpi;
pub use crate::metal::firmware::devicetree::{
    self,
    memory::{DtbMemory, Reservation, Source},
};
pub use crate::metal::firmware::map::{FirmwareMap, FirmwareRegion, MapError};
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub use crate::metal::firmware::uefi::{AcpiInput, DeviceTree, Loader, major};
#[cfg(target_arch = "riscv64")]
pub use crate::metal::{
    execution::description::{RiscvExtension, RiscvIsa},
    firmware::sbi::SbiServices,
    hart::description::{RiscvCpu, RiscvCpus},
    mmu::internal::description::{RiscvMmu, RiscvMmuMode},
    time::RiscvTimebase,
};
#[cfg(target_arch = "aarch64")]
pub use crate::metal::{
    firmware::{
        arm,
        psci::{Conduit, PsciDescription, PsciError, PsciVersion},
    },
    hart::{Aarch64Cpus, CpuCandidate},
};
pub use handoff::Boot;
pub(crate) use handoff::Descriptions;
pub use major::stack_range;
pub use physical::{Condition, Exclusion, Exclusions, Occupant, Retirement};
pub use regions::{RegionError, Regions};

/// The firmware adapter supplies a stable DTB (OpenSBI) or Loader (UEFI).
/// All referenced storage and mappings remain reserved until the consumer is done.
pub(crate) unsafe extern "C" fn start(hart: usize, input: *const ()) -> ! {
    // The nonreturning platform callback borrows the original descriptions
    // from here. Keep a single retained copy off the nested admission frames.
    let mut descriptions = Descriptions::new();
    let boot = unsafe { major::admit(hart, input, &mut descriptions) };
    if !boot.stacks_in_image() {
        Services::active().reject(format_args!(
            "FAIL Limen boot: stacks outside physical image\n"
        ));
    }
    unsafe { limen_platform_ready(boot) }
}

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

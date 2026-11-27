#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(
    platform::Platform {
        iommu: platform::Iommu::AmdVi,
        ..platform::Platform::native(4)
    }
    .device(platform::Device::Edu {
        slot: 6,
        function: 0,
        dma_mask: platform::DmaMask::Default,
    })
);

#[path = "../support/discovery/harness.rs"]
mod support;

use kernel::entry::boot::Boot;
use uefi::prelude::*;
use uefi_fixture as fixture;

#[path = "../../metal/memory/mmu/x86_64/external/amdvi/native/mod.rs"]
mod native;

#[entry]
fn main() -> Status {
    fixture::load(fixture::q35::calibrate)
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    // The native scenario consumes this boot admission once. There is no
    // independently collected case sharing its device state.
    native::check(boot);
    support::finish()
}

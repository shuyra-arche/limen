#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

use uefi_fixture as fixture;

#[path = "../../../metal/memory/mmu/x86_64/external/vtd/native/mod.rs"]
mod native;

guest::entry! {
    platform: platform::Platform {
        iommu: platform::Iommu::Vtd,
        ..platform::Platform::native(4)
    }
    .device(platform::Device::Edu {
        slot: 6,
        function: 0,
        dma_mask: platform::DmaMask::Default,
    })
    .device(platform::Device::Edu {
        slot: 7,
        function: 0,
        dma_mask: platform::DmaMask::Default,
    }),
    prepare: fixture::q35::calibrate,
    run: |boot| {
        // The native scenario consumes this boot admission once. There is no
        // independently collected case sharing its device state.
        native::check(boot);
        guest::finish()
    }
}

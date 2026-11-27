#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/memory/mmu/aarch64/external/smmu/native/mod.rs"]
mod native;

guest::entry! {
    platform: platform::Platform {
        iommu: platform::Iommu::SmmuV3,
        ..platform::Platform::native(4)
    }
    .device(platform::Device::Edu {
        slot: 6,
        function: 0,
        dma_mask: platform::DmaMask::Full32,
    })
    .device(platform::Device::Edu {
        slot: 7,
        function: 0,
        dma_mask: platform::DmaMask::Full32,
    }),
    run: |boot| {
        // The native scenario consumes this boot admission once. There is no
        // independently collected case sharing its device state.
        native::check(boot);
        guest::finish()
    }
}

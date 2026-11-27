#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/memory/mmu/riscv64/external/iommu/native.rs"]
mod native;

guest::entry! {
    platform: platform::Platform {
        iommu: platform::Iommu::Riscv,
        ..platform::Platform::native(1)
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
    test: |boot| {
        assert_eq!(boot.hart(), 0);
        assert!(boot.riscv_cpus().cpu(1).unwrap().is_none());
        assert_bare_translation();
        native::check(boot);
    },
}

fn assert_bare_translation() {
    let satp: usize;
    unsafe { core::arch::asm!("csrr {}, satp", out(reg) satp, options(nostack, nomem)) };
    assert_eq!(satp >> 60, 0);
}

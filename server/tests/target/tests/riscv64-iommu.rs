#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(
    platform::Platform {
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
    })
);

#[path = "../support/discovery/mod.rs"]
mod support;

use kernel::entry::boot::Boot;

#[path = "../../metal/memory/mmu/riscv64/external/iommu/native.rs"]
mod native;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    assert_eq!(boot.hart(), 0);
    assert!(boot.riscv_cpus().cpu(1).unwrap().is_none());
    collected();
    native::check(boot);
    support::finish()
}

#[test_case]
fn cpu_translation_starts_bare() {
    let satp: usize;
    unsafe { core::arch::asm!("csrr {}, satp", out(reg) satp, options(nostack, nomem)) };
    assert_eq!(satp >> 60, 0);
}

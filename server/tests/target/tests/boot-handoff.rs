#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::{Boot, platform};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

#[cfg(target_arch = "riscv64")]
#[path = "../../entry/boot/sbi/mod.rs"]
mod native;
#[cfg(target_os = "uefi")]
#[path = "../../entry/boot/uefi/mod.rs"]
mod native;

fn check_resident<T>(boot: &Boot<'_>, description: &T) {
    let start = core::ptr::from_ref(description).addr();
    let end = start.checked_add(size_of::<T>()).unwrap();
    let physical = PhysicalRange::new(
        PhysicalAddress::new(start as u64),
        PhysicalAddress::new(end as u64),
    )
    .unwrap();
    let original = boot.stack_range();
    assert!(boot.image().covers(physical));
    assert!(end <= original.start || original.end <= start);
}

guest::entry! {
    platform: platform::Platform::native(4),
    prepare: native::prepare,
    test: |boot| {
        #[cfg(any(target_arch = "riscv64", target_arch = "aarch64"))]
        check_resident(&boot, boot.dtb_memory().expect("retained DTB memory"));
        #[cfg(target_arch = "riscv64")]
        check_resident(&boot, boot.riscv_cpus());
        #[cfg(target_arch = "x86_64")]
        check_resident(&boot, boot.acpi().expect("retained Q35 ACPI"));

        #[cfg(target_arch = "riscv64")]
        {
            native::check(boot);
        }
        #[cfg(target_os = "uefi")]
        native::check(boot, guest::finish)
    },
}

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[cfg(target_arch = "riscv64")]
#[path = "../../entry/boot/sbi/mod.rs"]
mod native;
#[cfg(target_os = "uefi")]
#[path = "../../entry/boot/uefi/mod.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(4),
    prepare: native::prepare,
    run: |boot| {
        #[cfg(target_arch = "riscv64")]
        {
            native::check(boot);
            guest::finish()
        }
        #[cfg(target_os = "uefi")]
        native::check(boot, guest::finish)
    }
}

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[cfg(target_arch = "riscv64")]
#[path = "../support/sbi/execution.rs"]
mod native;
#[cfg(target_os = "uefi")]
#[path = "../support/uefi/execution.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(4),
    prepare: native::prepare,
    run: |boot| {
        native::check(boot, guest::finish)
    }
}

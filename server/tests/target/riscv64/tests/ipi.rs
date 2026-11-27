#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/interrupt/riscv64/internal/native_ipi.rs"]
mod scenario;

guest::entry! {
    platform: platform::Platform::native(2),
    run: |boot| {
        collected();
        scenario::check(boot);
        guest::finish()
    }
}

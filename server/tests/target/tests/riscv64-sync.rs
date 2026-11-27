#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../metal/sync/native.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(4),
    run: |_boot| {
        collected();
        guest::finish()
    }
}

#[test_case]
fn original_synchronization_contract() {
    native::check();
}

#[test_case]
fn original_enabled_interrupt_contract() {
    native::check_enabled();
}

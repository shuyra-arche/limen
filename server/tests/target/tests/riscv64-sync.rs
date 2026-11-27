#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/discovery/mod.rs"]
mod support;

use kernel::entry::boot::Boot;

#[path = "../../metal/sync/native.rs"]
mod native;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(_boot: Boot<'_>) -> ! {
    collected();
    support::finish()
}

#[test_case]
fn original_synchronization_contract() {
    native::check();
}

#[test_case]
fn original_enabled_interrupt_contract() {
    native::check_enabled();
}

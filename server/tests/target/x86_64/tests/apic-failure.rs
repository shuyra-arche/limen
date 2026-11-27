#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

use uefi_fixture as fixture;

#[path = "../../../metal/interrupt/x86_64/internal/apic/native_failure.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(4),
    prepare: fixture::q35::prepare_minor,
    run: |boot| {
        native::check(boot);
        guest::finish()
    }
}

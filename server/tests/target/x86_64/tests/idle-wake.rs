#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/interrupt/x86_64/internal/apic/native_wake.rs"]
mod scenario;

guest::entry! {
    platform: platform::Platform::native(2),
    prepare: uefi_fixture::q35::prepare_minor,
    test: scenario::check,
}

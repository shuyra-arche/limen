#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/interrupt/x86_64/internal/apic/native_timer.rs"]
mod timer;

guest::entry! {
    platform: platform::Platform::native(4),
    prepare: uefi_fixture::q35::calibrate,
    test: |boot| {
        uefi_fixture::check_handoff(&boot, None);
        timer::observe_tsc();
        timer::check(&boot);
    },
}

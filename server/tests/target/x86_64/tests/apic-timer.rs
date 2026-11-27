#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/interrupt/x86_64/internal/apic/native_timer.rs"]
mod timer;

guest::entry! {
    platform: platform::Platform::native(4),
    run: |boot| {
        uefi_fixture::check_handoff(&boot, None);
        collected();
        timer::check(&boot);
        guest::finish()
    }
}

#[test_case]
fn tsc_requires_stable_frequency() {
    timer::observe_tsc();
}

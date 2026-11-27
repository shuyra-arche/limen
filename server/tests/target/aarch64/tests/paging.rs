#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/memory/mmu/aarch64/paging.rs"]
mod scenario;

guest::entry! {
    platform: platform::Platform::native(4),
    test: |boot| {
        uefi_fixture::check_handoff(&boot, None);
        scenario::check(uefi_fixture::processor());
    },
}

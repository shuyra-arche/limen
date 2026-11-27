#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../support/paging/mod.rs"]
mod pages;
use pages::{DATA, LAST, MIDDLE, NEXT, Page, TOP, physical};

#[path = "../../../metal/memory/mmu/aarch64/paging.rs"]
mod scenario;

guest::entry! {
    platform: platform::Platform::native(4),
    run: |boot| {
        uefi_fixture::check_handoff(&boot, None);
        collected();
        guest::finish()
    }
}

#[test_case]
fn native_translation_roundtrip() {
    scenario::check(uefi_fixture::hart());
}

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
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
        scenario::check(uefi_fixture::hart());
        guest::finish()
    }
}

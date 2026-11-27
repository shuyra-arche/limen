#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(platform::Platform::native(4));

#[path = "../support/discovery/mod.rs"]
mod support;

use kernel::entry::boot::Boot;

#[path = "../support/paging/mod.rs"]
mod pages;
use pages::{DATA, LAST, MIDDLE, NEXT, Page, TOP, physical};

#[path = "../../metal/memory/mmu/aarch64/paging.rs"]
mod scenario;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    uefi_fixture::check_handoff(&boot, None);
    collected();
    support::finish()
}

#[test_case]
fn native_translation_roundtrip() {
    scenario::check(uefi_fixture::hart());
}

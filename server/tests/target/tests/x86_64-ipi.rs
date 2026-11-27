#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(platform::Platform::native(2));

use kernel::entry::boot::Boot;
use uefi::prelude::*;
use uefi_fixture as fixture;

#[path = "../../metal/interrupt/x86_64/internal/apic/native_ipi.rs"]
mod scenario;
#[path = "../support/discovery/harness.rs"]
mod support;

#[entry]
fn main() -> Status {
    fixture::load(fixture::q35::prepare_minor)
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    collected();
    scenario::check(boot);
    support::finish()
}

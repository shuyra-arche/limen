#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(collection::run)]
#![reexport_test_harness_main = "collected"]

use core::fmt::Write;
use kernel::{entry::boot::Boot, metal::platform::boot::treaty as diagnostic};
use uefi::prelude::*;
use uefi_fixture as fixture;

#[path = "../support/discovery/collection.rs"]
mod collection;
#[path = "../support/outcome/mod.rs"]
mod outcome;
#[path = "../../metal/interrupt/x86_64/internal/apic/native_ipi.rs"]
mod scenario;

#[entry]
fn main() -> Status {
    fixture::load(fixture::q35::prepare_minor)
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    collected();
    scenario::check(boot);
    outcome::finish(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(mut console) = diagnostic::console() {
        let _ = writeln!(console, "{info}");
    }
    outcome::finish(false)
}

#![no_std]
#![no_main]

use kernel::{entry::boot::Boot, metal::platform::boot::treaty as diagnostic};
use uefi::prelude::*;
use uefi_fixture as fixture;

#[path = "../../../../../metal/interrupt/x86_64/internal/apic/native_ipi.rs"]
mod scenario;

#[entry]
fn main() -> Status {
    fixture::load(fixture::q35::prepare_minor)
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    scenario::check(boot);
    diagnostic::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

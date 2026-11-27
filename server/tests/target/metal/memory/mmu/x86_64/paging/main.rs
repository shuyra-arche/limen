#![no_std]
#![no_main]

use kernel::{entry::boot::Boot, metal::platform::boot::treaty as diagnostic};
use uefi::prelude::*;
use uefi_fixture as fixture;

#[path = "../../../../../../metal/memory/mmu/x86_64/paging.rs"]
mod native;

#[entry]
fn main() -> Status {
    fixture::load(|| Ok(()))
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    native::check(boot);
    diagnostic::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

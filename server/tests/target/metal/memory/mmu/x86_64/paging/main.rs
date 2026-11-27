#![no_std]
#![no_main]

use kernel::entry::boot;
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use uefi::prelude::*;
use uefi_fixture as fixture;

#[path = "../../../../../support/paging/mod.rs"]
mod pages;
use pages::{DATA, LAST, MIDDLE, NEXT, Page, TOP, physical};

mod scenario;

#[entry]
fn main() -> Status {
    fixture::load(|| Ok(()))
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    fixture::check_handoff(&boot, None);
    let (apic, version) = {
        let mut fixed = fixture::q35::local_window(&boot);
        (fixed.physical(), unsafe { fixed.read_u32(0x30) }.unwrap())
    };
    scenario::check(boot.hart(), apic, version);
    boot_diagnostic::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

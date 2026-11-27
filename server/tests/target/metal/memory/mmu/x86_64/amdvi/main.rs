#![no_std]
#![no_main]

use kernel::entry::boot;
use uefi::prelude::*;
use uefi_fixture as fixture;

#[path = "../../../../../support/pci/q35.rs"]
mod edu;

mod scenario;

#[entry]
fn main() -> Status {
    fixture::load(fixture::q35::calibrate)
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    fixture::check_handoff(&boot, None);
    scenario::check(
        boot.acpi()
            .unwrap()
            .ivhd()
            .expect("valid IVRS")
            .expect("AMD remapping hardware"),
    );
    boot::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

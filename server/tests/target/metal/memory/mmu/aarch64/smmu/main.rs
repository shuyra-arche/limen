#![no_std]
#![no_main]

use kernel::entry::boot;
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use uefi::prelude::*;
use uefi_fixture as fixture;

mod scenario;

#[entry]
fn main() -> Status {
    fixture::load(|| Ok(()))
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    fixture::check_handoff(&boot, None);
    scenario::check(&boot.device_tree_description().unwrap());
    boot_diagnostic::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

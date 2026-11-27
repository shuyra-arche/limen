#![no_std]
#![no_main]

use kernel::entry::boot;
use uefi::prelude::*;
use uefi_fixture as fixture;

use fixture::{interrupt, q35};

mod scenario;

#[entry]
fn main() -> Status {
    fixture::load(|| Ok(()))
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    fixture::check_handoff(&boot, None);
    let mut window = q35::local_window(&boot);
    unsafe { interrupt::prepare(&mut window) };
    let mut controller = q35::local_controller(&boot, window);
    scenario::check(
        boot.acpi().unwrap().madt().unwrap(),
        boot.hart(),
        &mut controller,
    );
    boot::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

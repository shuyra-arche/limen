#![no_std]
#![no_main]

use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::Minor;

use kernel::entry::boot;
use uefi::prelude::*;
use uefi_fixture as fixture;

use fixture::q35::delay;

mod scenario;

static MINOR: Minor = Minor::new();

#[entry]
fn main() -> Status {
    fixture::load(fixture::q35::prepare_minor)
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    let view = fixture::check_handoff(&boot, Some(&MINOR));
    let mut startup = fixture::inspect(&boot, &view);
    scenario::check_failure(&mut startup, fixture::target());
    // Keep the original uncertain launch resident through shutdown.
    boot_diagnostic::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

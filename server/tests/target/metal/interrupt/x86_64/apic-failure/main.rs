#![no_std]
#![no_main]

use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::Minor;
use kernel::metal::{
    processor::treaty::{LocalStorage, Stack},
    sync::treaty::Supply,
};

use kernel::entry::boot;
use uefi::prelude::*;
use uefi_fixture as fixture;

use fixture::q35::delay;

mod scenario;

static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);

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

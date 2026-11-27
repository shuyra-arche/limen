#![no_std]
#![no_main]

use kernel::entry::boot;
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use uefi::prelude::*;
use uefi_fixture as fixture;

#[path = "../../../../../metal/interrupt/x86_64/internal/apic/native_timer.rs"]
mod timer;

#[entry]
fn main() -> Status {
    fixture::load(|| Ok(()))
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    fixture::check_handoff(&boot, None);
    timer::observe_tsc();
    timer::check(&boot);
    boot_diagnostic::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

#![no_std]
#![no_main]

use core::fmt::Write;
use kernel::{entry::boot::Boot, metal::platform::boot::treaty as diagnostic};

kernel::limen_riscv_major!(stack: 32 * 1024, nodes: 5);

#[path = "../../../../../metal/interrupt/riscv64/external/native_aplic.rs"]
mod scenario;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    scenario::check(boot);
    diagnostic::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(mut console) = diagnostic::console() {
        let _ = writeln!(console, "FAIL rv64-interrupt {info}");
    }
    diagnostic::shutdown(false)
}

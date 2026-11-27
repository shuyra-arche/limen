#![no_std]
#![no_main]

use core::fmt::Write;
use kernel::{entry::boot::Boot, metal::platform::boot::treaty as diagnostic};

#[path = "../../../basis/execution/participation.rs"]
mod native;

kernel::limen_riscv_major!(stack: native::MAJOR_BYTES, nodes: native::MAJOR_NODES);

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    native::check(boot, || diagnostic::shutdown(true))
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(mut console) = diagnostic::console() {
        let _ = writeln!(console, "FAIL kernel-execution-harts: {info}");
    }
    diagnostic::shutdown(false)
}

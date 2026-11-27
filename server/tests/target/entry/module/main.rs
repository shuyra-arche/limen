#![no_std]
#![no_main]

use kernel::{entry::boot::Boot, metal::platform::boot::treaty as diagnostic};
use uefi::prelude::*;

#[path = "../../../metal/module/retention/mod.rs"]
mod native;

#[entry]
fn main() -> Status {
    native::load()
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    native::check(boot);
    diagnostic::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    native::fail(info)
}

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[cfg(target_arch = "aarch64")]
const FIRMWARE: platform::Firmware = platform::Firmware::AavmfStrictNx;
#[cfg(target_arch = "x86_64")]
const FIRMWARE: platform::Firmware = platform::Firmware::OvmfStrictNx;

guest::declare!(platform::Platform {
    firmware: FIRMWARE,
    ..platform::Platform::native(1)
});

use kernel::entry::boot::Boot;
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
    guest::finish()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    native::report(info);
    guest::fail()
}

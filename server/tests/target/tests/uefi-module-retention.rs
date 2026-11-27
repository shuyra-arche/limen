#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(collection::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;

#[cfg(target_arch = "aarch64")]
const FIRMWARE: platform::Firmware = platform::Firmware::AavmfStrictNx;
#[cfg(target_arch = "x86_64")]
const FIRMWARE: platform::Firmware = platform::Firmware::OvmfStrictNx;

platform::declare!(platform::Platform {
    firmware: FIRMWARE,
    ..platform::Platform::native(1)
});

use kernel::entry::boot::Boot;
use uefi::prelude::*;

#[path = "../support/discovery/collection.rs"]
mod collection;
#[path = "../../metal/module/retention/mod.rs"]
mod native;
#[path = "../support/outcome/mod.rs"]
mod outcome;

#[entry]
fn main() -> Status {
    native::load()
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    native::check(boot);
    outcome::finish(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    native::report(info);
    outcome::finish(false)
}

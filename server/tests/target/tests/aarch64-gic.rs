#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(platform::Platform {
    serial: platform::Serial::Second,
    ..platform::Platform::native(4)
});

#[path = "../support/discovery/mod.rs"]
mod support;

use kernel::entry::boot::Boot;

#[path = "../../metal/interrupt/aarch64/gic/native/mod.rs"]
mod native;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    collected();
    native::check(boot);
    support::finish()
}

#[test_case]
fn firmware_peer_is_distinct() {
    assert_ne!(uefi_fixture::target(), uefi_fixture::hart());
}

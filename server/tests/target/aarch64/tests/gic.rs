#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/interrupt/aarch64/gic/native/mod.rs"]
mod native;

guest::entry! {
    platform: platform::Platform {
        serial: platform::Serial::Second,
        ..platform::Platform::native(4)
    },
    test: |boot| {
        assert_ne!(uefi_fixture::target(), uefi_fixture::hart());
        native::check(boot);
    },
}

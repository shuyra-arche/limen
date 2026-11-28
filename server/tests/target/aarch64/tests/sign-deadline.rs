#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../basis/execution/aarch64_deadline.rs"]
mod native;

guest::entry! {
    platform: platform::Platform {
        serial: platform::Serial::Second,
        ..platform::Platform::native(4)
    },
    test: |boot| {
        native::check(boot, guest::finish)
    },
}

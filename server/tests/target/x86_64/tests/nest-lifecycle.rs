#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../entry/boot/uefi/nest.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(4),
    prepare: native::prepare,
    test: |boot| {
        native::check(boot, guest::finish)
    },
}

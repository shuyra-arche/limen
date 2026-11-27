#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../support/uefi/execution.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(4),
    prepare: native::prepare,
    run: |boot| {
        native::check(boot, guest::finish)
    }
}

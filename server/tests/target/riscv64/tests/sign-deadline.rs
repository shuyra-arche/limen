#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../basis/execution/deadline.rs"]
mod native;

guest::entry! {
    platform: platform::Platform {
        sstc: platform::Setting::On,
        ..platform::Platform::native(1)
    },
    test: |boot| {
        native::check(boot, None, guest::finish)
    },
}

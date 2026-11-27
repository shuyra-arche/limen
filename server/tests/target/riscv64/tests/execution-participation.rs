#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../basis/execution/participation.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(4),
    major: (native::MAJOR_BYTES, native::MAJOR_NODES),
    test: |boot| {
        native::check(boot, guest::finish)
    },
}

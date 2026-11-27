#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../basis/execution/participation.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(4),
    major: (native::MAJOR_BYTES, native::MAJOR_NODES),
    run: |boot| {
        native::check(boot, guest::finish)
    }
}

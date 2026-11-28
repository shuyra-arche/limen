#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../basis/execution/fund_smp.rs"]
mod native;

guest::entry! {
    platform: platform::Platform {
        sstc: platform::Setting::On,
        ..platform::Platform::native(2)
    },
    test: |boot| {
        native::check(boot, guest::finish)
    },
}

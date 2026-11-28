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
        aclint: true,
        sstc: platform::Setting::Off,
        ..platform::Platform::native(1)
    },
    test: |boot| {
        let service = boot.sbi_services().timer().unwrap().expect("SBI TIME");
        native::check(boot, Some(service), guest::finish)
    },
}

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/memory/mmu/x86_64/paging.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(4),
    test: |boot| {
        native::check(boot);
    },
}

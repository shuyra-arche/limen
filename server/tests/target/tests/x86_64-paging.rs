#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../metal/memory/mmu/x86_64/paging.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(4),
    run: |boot| {
        native::check(boot);
        guest::finish()
    }
}

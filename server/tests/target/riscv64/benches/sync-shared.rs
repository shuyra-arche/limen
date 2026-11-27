//! Contended and alternating synchronization on four actual Harts.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/sync/bench/shared.rs"]
mod sync;

guest::entry! {
    platform: platform::Platform::native(4),
    run: |boot| {
        sync::check(boot);
        guest::finish()
    }
}

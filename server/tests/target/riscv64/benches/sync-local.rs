//! Uncontended synchronization on a one-Hart machine.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../../benches/metal/sync/local.rs"]
mod sync;

guest::entry! {
    platform: platform::Platform::native(1),
    bench: sync::check,
}

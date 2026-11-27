//! A measurement transport smoke benchmark over the admitted local timebase.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../../benches/metal/chrono/clock.rs"]
mod clock;

guest::entry! {
    platform: platform::Platform::native(1),
    run: |boot| {
        clock::check(boot);
        guest::finish()
    }
}

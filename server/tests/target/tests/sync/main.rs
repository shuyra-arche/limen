#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[cfg(not(all(feature = "bench", target_arch = "riscv64")))]
mod contracts;

#[cfg(all(feature = "bench", target_arch = "riscv64"))]
#[path = "../../../../benches/metal/sync/shared/mod.rs"]
mod measurement;

guest::entry! {
    platform: platform::Platform::native(4),
    test: collected,
    #[cfg(target_arch = "riscv64")]
    bench: measurement::check,
}

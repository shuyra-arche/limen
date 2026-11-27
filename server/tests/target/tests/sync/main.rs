#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![cfg_attr(not(feature = "bench"), test_runner(guest::run))]
#![cfg_attr(feature = "bench", test_runner(guest::scenario))]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[cfg(not(feature = "bench"))]
mod contracts;

#[cfg(all(feature = "bench", not(target_arch = "riscv64")))]
compile_error!("Sync measurement currently requires riscv64");
#[cfg(all(feature = "bench", target_arch = "riscv64"))]
#[path = "../../../../benches/metal/sync/shared/mod.rs"]
mod measurement;

guest::entry! {
    platform: platform::Platform::native(4),
    test: |_boot| {},
    bench: |boot| {
        measurement::check(boot);
    }
}

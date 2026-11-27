#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../../support/discovery/mod.rs"]
mod support;

mod leaf;
#[cfg(not(target_arch = "riscv64"))]
#[path = "../../../metal/sync/native.rs"]
mod native;

use kernel::{entry::boot::Boot, metal::sync::treaty::TurnLock};

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(_boot: Boot<'_>) -> ! {
    collected();
    support::finish()
}

#[test_case]
fn turn_releases_the_original_value() {
    let lock = TurnLock::new(0_u64);
    assert_eq!(
        lock.acquire(|value| {
            *value += 1;
            *value
        }),
        Ok(1)
    );
    assert_eq!(lock.acquire(|value| *value), Ok(1));
}

#[cfg(not(target_arch = "riscv64"))]
#[test_case]
fn original_synchronization_contract() {
    native::check();
}

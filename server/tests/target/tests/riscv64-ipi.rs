#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(platform::Platform::native(2));

#[path = "../support/discovery/mod.rs"]
mod support;

use kernel::entry::boot::Boot;

#[path = "../../metal/interrupt/riscv64/internal/native_ipi.rs"]
mod scenario;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    collected();
    scenario::check(boot);
    support::finish()
}

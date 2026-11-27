#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(platform::Platform::native(2));

use kernel::entry::boot::Boot;

#[path = "../../metal/module/retirement/mod.rs"]
mod native;
#[path = "../support/discovery/mod.rs"]
mod support;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    assert!(boot.riscv_cpus().cpu(2).unwrap().is_none());
    native::check(boot);
    support::finish()
}

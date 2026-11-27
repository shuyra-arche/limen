#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(platform::Platform::native(4));

use kernel::entry::boot::Boot;

#[path = "../../basis/execution/participation.rs"]
mod native;
#[path = "../support/discovery/harness.rs"]
mod support;

kernel::limen_riscv_major!(stack: native::MAJOR_BYTES, nodes: native::MAJOR_NODES);

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    native::check(boot, support::finish)
}

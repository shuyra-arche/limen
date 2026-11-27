#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(platform::Platform {
    interrupt: platform::Interrupt::Aplic,
    aclint: true,
    sstc: platform::Setting::Off,
    ..platform::Platform::native(1)
});

#[path = "../support/discovery/mod.rs"]
mod support;

use kernel::{entry::boot::Boot, metal::sync::treaty::turn_capacity};

#[path = "../../metal/interrupt/riscv64/external/native_aplic.rs"]
mod scenario;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    assert_eq!(boot.hart(), 0);
    assert!(boot.riscv_cpus().cpu(1).unwrap().is_none());
    collected();
    scenario::check(boot);
    support::finish()
}

#[test_case]
fn admitted_local_state_is_available() {
    assert_eq!(turn_capacity(), Some(5));
}

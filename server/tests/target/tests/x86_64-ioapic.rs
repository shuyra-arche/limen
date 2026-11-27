#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(platform::Platform {
    serial: platform::Serial::Isa,
    ..platform::Platform::native(4)
});

#[path = "../support/discovery/mod.rs"]
mod support;

use kernel::entry::boot::Boot;

#[path = "../../metal/interrupt/x86_64/external/ioapic/native/mod.rs"]
mod native;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    collected();
    native::check(boot);
    support::finish()
}

#[test_case]
fn local_interrupt_context_is_admitted() {
    assert_eq!(kernel::metal::sync::treaty::turn_capacity(), Some(5));
}

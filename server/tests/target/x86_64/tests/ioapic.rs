#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/interrupt/x86_64/external/ioapic/native/mod.rs"]
mod native;

guest::entry! {
    platform: platform::Platform {
        serial: platform::Serial::Isa,
        ..platform::Platform::native(4)
    },
    run: |boot| {
        collected();
        native::check(boot);
        guest::finish()
    }
}

#[test_case]
fn local_interrupt_context_is_admitted() {
    assert_eq!(kernel::metal::sync::treaty::turn_capacity(), Some(5));
}

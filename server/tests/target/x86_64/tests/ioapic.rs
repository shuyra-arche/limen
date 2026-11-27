#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
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
        assert_eq!(kernel::metal::sync::treaty::turn_capacity(), Some(5));
        native::check(boot);
        guest::finish()
    }
}

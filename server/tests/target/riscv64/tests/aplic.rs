#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

use kernel::metal::sync::treaty::turn_capacity;

#[path = "../../../metal/interrupt/riscv64/external/native_aplic.rs"]
mod scenario;

guest::entry! {
    platform: platform::Platform {
        interrupt: platform::Interrupt::Aplic,
        aclint: true,
        sstc: platform::Setting::Off,
        ..platform::Platform::native(1)
    },
    run: |boot| {
        assert_eq!(boot.hart(), 0);
        assert!(boot.riscv_cpus().cpu(1).unwrap().is_none());
        collected();
        scenario::check(boot);
        guest::finish()
    }
}

#[test_case]
fn admitted_local_state_is_available() {
    assert_eq!(turn_capacity(), Some(5));
}

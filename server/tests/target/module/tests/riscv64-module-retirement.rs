#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/module/retirement/mod.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(2),
    run: |boot| {
        assert!(boot.riscv_cpus().cpu(2).unwrap().is_none());
        native::check(boot);
        guest::finish()
    }
}

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/platform.rs"]
mod platform;
platform::declare!(platform::Platform {
    sstc: platform::Setting::On,
    ..platform::Platform::native(2)
});

#[path = "../support/discovery/mod.rs"]
mod support;

use kernel::entry::boot::Boot;

#[path = "../../metal/interrupt/riscv64/external/native_plic.rs"]
mod plic;
#[path = "../support/riscv64/reception.rs"]
mod reception;
#[path = "../../metal/interrupt/riscv64/internal/timer/native_sstc.rs"]
mod sstc;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(mut boot: Boot<'_>) -> ! {
    assert!(boot.riscv_cpus().cpu(0).unwrap().unwrap().enabled());
    assert!(boot.riscv_cpus().cpu(1).unwrap().unwrap().enabled());
    assert!(boot.riscv_cpus().cpu(2).unwrap().is_none());
    let mut reception = boot.take_reception().unwrap();
    plic::check(&boot, &mut reception);
    sstc::check(&boot, &mut reception.timer);
    support::finish()
}

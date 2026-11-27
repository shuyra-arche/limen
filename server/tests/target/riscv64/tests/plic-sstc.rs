#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/interrupt/riscv64/external/native_plic.rs"]
mod plic;
#[path = "../../support/riscv64/reception.rs"]
mod reception;
#[path = "../../../metal/interrupt/riscv64/internal/timer/native_sstc.rs"]
mod sstc;

guest::entry! {
    platform: platform::Platform {
        sstc: platform::Setting::On,
        ..platform::Platform::native(2)
    },
    test: |mut boot| {
        assert!(boot.riscv_cpus().cpu(0).unwrap().unwrap().enabled());
        assert!(boot.riscv_cpus().cpu(1).unwrap().unwrap().enabled());
        assert!(boot.riscv_cpus().cpu(2).unwrap().is_none());
        let mut reception = boot.take_reception().unwrap();
        plic::check(&boot, &mut reception);
        sstc::check(&boot, &mut reception.timer);
    },
}

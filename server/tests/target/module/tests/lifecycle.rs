#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[cfg(target_arch = "riscv64")]
#[path = "../../../metal/module/retirement/mod.rs"]
mod retirement;

#[cfg(target_arch = "riscv64")]
guest::entry! {
    platform: platform::Platform::native(2),
    run: |boot| {
        assert!(boot.riscv_cpus().cpu(2).unwrap().is_none());
        retirement::check(boot);
        guest::finish()
    }
}

// Strict NX requires a dedicated UEFI loader and pre-admission diagnostics.
// It shares the platform record and completion, not the standard loader.
#[cfg(target_os = "uefi")]
#[path = "../../../metal/module/retention/mod.rs"]
mod retention;

#[cfg(target_arch = "aarch64")]
const FIRMWARE: platform::Firmware = platform::Firmware::AavmfStrictNx;
#[cfg(target_arch = "x86_64")]
const FIRMWARE: platform::Firmware = platform::Firmware::OvmfStrictNx;

#[cfg(target_os = "uefi")]
guest::declare!(platform::Platform {
    firmware: FIRMWARE,
    ..platform::Platform::native(1)
});

#[cfg(target_os = "uefi")]
#[uefi::entry]
fn main() -> uefi::Status {
    retention::load()
}

#[cfg(target_os = "uefi")]
#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: guest::Boot<'_>) -> ! {
    collected();
    retention::check(boot);
    guest::finish()
}

#[cfg(target_os = "uefi")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    retention::report(info);
    guest::fail()
}

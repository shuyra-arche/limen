#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../basis/execution/lifecycle/mod.rs"]
mod native;

#[cfg(target_os = "uefi")]
fn prepare() -> Result<(), uefi::Status> {
    #[cfg(target_arch = "x86_64")]
    uefi_fixture::q35::calibrate()?;
    Ok(())
}

guest::entry! {
    platform: platform::Platform::native(4),
    prepare: prepare,
    test: |boot| {
        native::check(boot, guest::finish)
    },
}

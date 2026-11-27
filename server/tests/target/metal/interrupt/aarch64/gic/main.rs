#![no_std]
#![no_main]

use kernel::entry::boot;
use uefi::prelude::*;
use uefi_fixture as fixture;

use kernel::metal::{chrono::aarch64::GenericCounter, interrupt::aarch64::gic::GicV3};

#[path = "../../../../support/clock.rs"]
mod clock;

mod scenario;

#[entry]
fn main() -> Status {
    fixture::load(|| Ok(()))
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    fixture::check_handoff(&boot, None);
    let tree = boot.device_tree_description().unwrap();
    let description = GenericCounter::read(&tree).unwrap().unwrap();
    let local = unsafe { description.clock(boot.hart()) }.unwrap();
    let distinct = unsafe { description.clock(boot.hart()) }.unwrap();
    assert!(
        local
            .sample()
            .unwrap()
            .elapsed_since(distinct.sample().unwrap())
            .is_none()
    );
    clock::observe(&local);
    let controller = GicV3::read(&tree).unwrap().expect("GIC description");
    scenario::check(&tree, controller, boot.hart());
    boot::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

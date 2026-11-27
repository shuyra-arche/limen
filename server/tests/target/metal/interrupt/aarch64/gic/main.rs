#![no_std]
#![no_main]

use kernel::entry::boot;
use uefi::prelude::*;
use uefi_fixture as fixture;

use core::time::Duration;
use kernel::metal::{
    chrono::{Clock, aarch64::GenericCounter},
    interrupt::aarch64::gic::GicV3,
    processor::hart::HartId,
};

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
    assert!(unsafe { description.bind(boot.hart() ^ 1) }.is_err());
    let local = unsafe { description.bind(boot.hart()) }.unwrap();
    let distinct = unsafe { description.bind(boot.hart()) }.unwrap();
    let hart = HartId::new(boot.hart());
    assert_eq!(local.clock().hart(), hart.get());
    let supplied = Clock::new(hart, local.clock().frequency());
    let deadline = supplied
        .observe(hart, || local.clock().read().unwrap())
        .unwrap()
        .after(Duration::from_millis(1))
        .unwrap();
    assert!(local.clock().count_for(deadline).is_none());
    assert!(
        local
            .clock()
            .sample()
            .unwrap()
            .elapsed_since(distinct.clock().sample().unwrap())
            .is_none()
    );
    clock::observe(local.clock());
    let controller = GicV3::read(&tree).unwrap().expect("GIC description");
    scenario::check(&tree, controller, boot.hart());
    boot::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

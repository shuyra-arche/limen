#![no_std]
#![no_main]

use kernel::entry::boot;
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use uefi::prelude::*;
use uefi_fixture as fixture;

use kernel::metal::{
    chrono::aarch64::description::GenericCounter, interrupt::aarch64::gic::GicV3,
    processor::treaty::HartId,
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
    boot_diagnostic::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

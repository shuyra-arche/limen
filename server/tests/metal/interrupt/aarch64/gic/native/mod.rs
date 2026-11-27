use kernel::entry::boot;
use uefi_fixture as fixture;

use kernel::metal::{
    chrono::aarch64::description::GenericCounter, interrupt::aarch64::gic::GicV3,
    processor::treaty::HartId,
};

#[path = "../../../../chrono/observation.rs"]
mod clock;

mod scenario;

pub fn check(boot: boot::Boot<'_>) {
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
}

use kernel::entry::boot;
use kernel::metal::platform::description::devicetree::DeviceTree;

#[path = "stream.rs"]
mod stream;

pub(super) fn check(tree: &DeviceTree<'_>) {
    use kernel::metal::memory::mmu::{
        aarch64::external::smmu::{self, Controller, DeviceOrder},
        access::device::DeviceWindow,
    };
    use limen_treaty::model::InternalAddress;
    let mut range = None;
    for node in tree.nodes() {
        if let Some(registers) = smmu::registers(node).unwrap() {
            assert!(range.replace(registers).is_none());
        }
    }
    let range = range.expect("virt SMMUv3");
    // AAVMF retains this device mapping after ExitBootServices. The guest
    // owns the test endpoint and keeps the firmware disk quiescent after exit.
    let mut registers =
        unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())).unwrap() };
    let mut controller =
        unsafe { Controller::admit(&mut registers, DeviceOrder::native()) }.unwrap();
    let profile = controller.profile();
    assert!(profile.stage_two && profile.granule_4k && !profile.enabled);
    let mut endpoint = stream::Endpoint::open(tree, range);
    endpoint.prime();
    unsafe { controller.begin_abort_bypass() }.unwrap();
    fn retry<T>(mut f: impl FnMut() -> Result<T, smmu::Error>) -> T {
        for _ in 0..100_000 {
            match f() {
                Ok(value) => return value,
                Err(smmu::Error::Busy) => core::hint::spin_loop(),
                Err(e) => panic!("SMMUv3 control failed: {e:?}"),
            }
        }
        panic!("SMMUv3 acknowledgement timeout");
    }
    retry(|| controller.observe_abort());
    endpoint.verify_abort();
    endpoint.install(&mut controller);
    use limen_treaty::model::PhysicalAddress;
    use smmu::queue::{Invalidate, Ring};
    static mut COMMANDS: Ring = Ring::EMPTY;
    let commands = unsafe { &mut *(&raw mut COMMANDS) };
    let physical = PhysicalAddress::new(core::ptr::from_ref(commands).addr() as u64);
    unsafe { controller.open_queue(commands, physical) }.unwrap();
    for operation in [
        Invalidate::Stream(stream::RID),
        Invalidate::StageTwo(1),
        Invalidate::Stream(1),
        Invalidate::StageTwo(2),
    ] {
        retry(|| controller.submit(operation));
        assert_eq!(controller.submit(operation), Err(smmu::Error::Active));
        assert_eq!(controller.unsettled_invalidation(), Some(operation));
        assert_eq!(retry(|| controller.observe()), operation);
        assert_eq!(controller.observe(), Err(smmu::Error::Active));
    }
    endpoint.verify_translation();
    endpoint.downgrade();
    let invalidate = Invalidate::StageTwo(1);
    retry(|| controller.submit(invalidate));
    assert_eq!(retry(|| controller.observe()), invalidate);
    endpoint.verify_downgrade();
    retry(|| controller.shut_queue());
    boot::write("\nPASS smmuv3 stage2=1 granule=4096 abort=1 sync=5 dma=1 downgrade=1\n");
}

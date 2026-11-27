use kernel::metal::platform::boot::treaty as boot_diagnostic;
#[path = "../../../../../support/paging/binding.rs"]
mod binding;

use kernel::metal::memory::mmu::treaty::{BindingControl, BindingEffect, BindingRejection};
use kernel::metal::{
    memory::{
        access::treaty::device::DeviceWindow,
        mmu::aarch64::external::smmu::{
            self, Controller, DeviceOrder, Effect, Refresh, queue::Ring,
        },
    },
    platform::description::treaty::devicetree::DeviceTree,
};
use limen_treaty::model::{InternalAddress, PhysicalAddress};

#[path = "stream.rs"]
mod stream;
use stream::{IOVA, PATTERNS, order};

static mut COMMANDS: Ring = Ring::EMPTY;

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

fn check_destinations(endpoints: &mut [stream::Endpoint; 2], requester: usize, translated: bool) {
    let words = [endpoints[0].word(), endpoints[1].word()];
    for (word, pattern) in words.into_iter().zip(PATTERNS) {
        unsafe { word.write_volatile(!pattern) };
    }
    order();
    endpoints[requester].write(IOVA);
    for (index, (word, pattern)) in words.into_iter().zip(PATTERNS).enumerate() {
        let expected = if translated && index == requester {
            pattern
        } else {
            !pattern
        };
        assert_eq!(unsafe { word.read_volatile() }, expected);
    }
}

pub(super) fn check(tree: &DeviceTree<'_>) {
    let mut range = None;
    for node in tree.nodes() {
        if let Some(registers) = smmu::registers(node).unwrap() {
            assert!(range.replace(registers).is_none());
        }
    }
    let range = range.expect("virt SMMUv3");
    // The retained AAVMF mapping covers this exact physical register scope.
    let mut registers =
        unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())).unwrap() };
    let (storage, table) = unsafe { stream::directory() };
    let mut controller =
        unsafe { Controller::admit(&mut registers, DeviceOrder::native(), storage) }.unwrap();
    let profile = controller.profile();
    assert!(profile.stage_two && profile.granule_4k && !profile.enabled);
    let mut endpoints = [
        stream::Endpoint::open(tree, range, 0),
        stream::Endpoint::open(tree, range, 1),
    ];
    for endpoint in &mut endpoints {
        endpoint.prime();
    }
    unsafe { controller.begin_abort_bypass() }.unwrap();
    retry(|| controller.observe_abort());
    for endpoint in &mut endpoints {
        endpoint.verify_abort();
    }
    let bindings = [
        endpoints[0].prepare(&mut controller),
        endpoints[1].prepare(&mut controller),
    ];
    unsafe { controller.begin_install() }.unwrap();
    retry(|| controller.observe_install());
    assert_eq!(controller.retained_table(), Some(table));
    let commands = unsafe { &mut *(&raw mut COMMANDS) };
    let physical = PhysicalAddress::new(core::ptr::from_ref(commands).addr() as u64);
    unsafe { controller.open_queue(commands, physical) }.unwrap();
    let mut previous = None;
    for binding in bindings {
        for refresh in [Refresh::Stream, Refresh::StageTwo] {
            let command = unsafe { controller.begin_refresh(binding, refresh) }.unwrap();
            let effect = Effect::Refreshed(binding, refresh);
            assert_eq!(controller.unsettled_effect(), Some(effect));
            assert_eq!(
                unsafe { controller.begin_refresh(binding, refresh) },
                Err(smmu::Error::Busy)
            );
            if let Some(previous) = previous {
                assert_eq!(controller.observe(previous), Err(smmu::Error::Stale));
            }
            assert_eq!(retry(|| controller.observe(command)), effect);
            assert_eq!(controller.observe(command), Err(smmu::Error::Stale));
            previous = Some(command);
        }
    }
    check_destinations(&mut endpoints, 0, true);
    check_destinations(&mut endpoints, 1, true);
    endpoints[0].downgrade(controller.format().unwrap());
    let binding = bindings[0];
    let refresh = Refresh::StageTwo;
    let command =
        unsafe { BindingControl::begin_refresh(&mut controller, binding, refresh) }.unwrap();
    assert_eq!(
        binding::settle(&mut controller, command),
        BindingEffect::Refreshed(binding, refresh)
    );
    check_destinations(&mut endpoints, 0, false);
    check_destinations(&mut endpoints, 1, true);
    let previous = command.command();
    let (ring, queue) = retry(|| controller.shut_queue());
    assert_eq!(queue, physical);
    unsafe { controller.open_queue(ring, queue) }.unwrap();
    let command = unsafe { controller.begin_retirement(bindings[1]) }.unwrap();
    assert_eq!(controller.observe(previous), Err(smmu::Error::Stale));
    assert_eq!(controller.binding(stream::SECOND), None);
    assert_eq!(controller.pending_command(), Some(command.command()));
    assert!(controller.pending_change(bindings[0]).is_none());
    assert_eq!(
        binding::settle(&mut controller, command),
        BindingEffect::Unbound(bindings[1])
    );
    assert!(controller.pending_command().is_none());
    check_destinations(&mut endpoints, 1, false);
    let replacement = unsafe { binding::bind(&mut controller, bindings[1].context()) };
    assert_ne!(replacement, bindings[1]);
    assert_eq!(
        unsafe { controller.begin_retirement(bindings[1]) },
        Err(BindingRejection::Rejected(smmu::Error::Stale))
    );
    check_destinations(&mut endpoints, 1, true);
    for binding in [bindings[0], replacement] {
        unsafe { binding::retire(&mut controller, binding, |_| {}) };
    }
    let (returned, original) = retry(|| controller.shut_queue());
    assert_eq!(original, physical);
    assert_eq!(core::ptr::from_ref(returned).addr() as u64, original.get());
    assert_eq!(controller.retained_queue(), None);
    unsafe { controller.begin_shut() }.unwrap();
    retry(|| controller.observe_shut());
    assert_eq!(controller.retained_table(), Some(table));
    assert_eq!(
        unsafe { controller.begin_install() },
        Err(smmu::Error::Active)
    );
    boot_diagnostic::write(
        "\nPASS smmuv3 stage2=1 granule=4096 abort=1 sync=5 dma=1 downgrade=1 bindings=2 unbound=1 stale=1 recovered=1 queue-return=2 stopped=1\n",
    );
}

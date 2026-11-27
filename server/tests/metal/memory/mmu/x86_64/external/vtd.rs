use super::super::test_firmware::Firmware;
use super::{
    CAPABILITY, CONTEXT, Controller, EXTENDED, Error, IN_PROGRESS, State,
    binding::{Context, Sequence},
    command::{Change, CommandId, Operation},
    directory::{Storage, Table},
    translation::Visibility,
};
use crate::metal::memory::{
    access::treaty::device::DeviceWindow,
    mmu::treaty::{Retirement, Unbind},
};
use limen_treaty::model::{InternalAddress, PhysicalAddress};

#[test]
fn binding_keeps_its_retirement_number_when_the_sequence_is_exhausted() {
    let unit = PhysicalAddress::new(0xfed9_0000);
    let context = Context {
        requester: 8,
        domain: 1,
        root: PhysicalAddress::new(0x1000),
    };
    let mut sequence = Sequence(u64::MAX - 3);

    let binding = sequence.reserve_binding(unit, context).unwrap();
    assert_eq!(
        binding.publication,
        CommandId {
            unit,
            number: u64::MAX - 2,
        }
    );
    let retirement = binding.retirement();
    assert_eq!(
        retirement,
        CommandId {
            unit,
            number: u64::MAX - 1,
        }
    );

    // Failed admission leaves the final general number available.
    assert_eq!(
        sequence.reserve_binding(unit, context),
        Err(Error::Exhausted)
    );
    assert_eq!(sequence.reserve(1), Ok(u64::MAX));
    assert_eq!(sequence.reserve(1), Err(Error::Exhausted));
    assert_eq!(binding.retirement(), retirement);
}

#[test]
fn retirement_requires_the_original_context_and_iotlb_completions() {
    let mut description = [0; 64];
    description[..4].copy_from_slice(b"DMAR");
    description[36] = 47;
    description[48..53].copy_from_slice(&[0, 0, 16, 0, 1]);
    description[56..64].copy_from_slice(&0xfed9_0000u64.to_le_bytes());
    let firmware = Firmware::new(description);
    let unit = firmware
        .acpi()
        .dmar()
        .unwrap()
        .unwrap()
        .units()
        .next()
        .unwrap();
    let mut registers = [0u64; 4096 / 8];
    registers[0] = 0x10;
    registers[CAPABILITY / 8] = (1 << 9) | (38 << 16) | (6 << 24);
    registers[EXTENDED / 8] = 4 << 8;
    let base = registers.as_mut_ptr().expose_provenance() as u64;
    let mut window =
        unsafe { DeviceWindow::admit(unit.registers(), InternalAddress::new(base)) }.unwrap();
    let mut root = Table::EMPTY;
    let mut contexts = Table::EMPTY;
    let mut bindings = [None];
    let mut sequence = Sequence::FRESH;
    let storage = Storage {
        bus: 0,
        root: (&mut root, PhysicalAddress::new(0x8000)),
        contexts: (&mut contexts, PhysicalAddress::new(0x9000)),
        bindings: &mut bindings,
        sequence: &mut sequence,
    };
    let mut controller =
        unsafe { Controller::admit(unit, &mut window, storage, Visibility { line: 64 }) }.unwrap();
    let context = Context {
        requester: 0x30,
        domain: 1,
        root: PhysicalAddress::new(0xa000),
    };
    // Start after the denying context has already been published. Native
    // cache-line writeback and the actual start are covered by the q35 guest.
    let original = controller
        .storage
        .sequence
        .reserve_binding(unit.registers().start(), context)
        .unwrap();
    controller.storage.bindings[0] = Some(original);
    controller.pending = Some((original.retirement(), Operation::Unbind(0)));
    controller.state = State::RefreshingContexts;
    controller.write(CONTEXT, IN_PROGRESS);
    let ticket = controller.pending_retirement(original).unwrap();
    assert_eq!(
        controller.pending(),
        Some((original.retirement(), Change::Unbound(original)))
    );
    assert_eq!(controller.observe(original.publication), Err(Error::Stale));
    assert_eq!(
        controller.observe_retirement(&ticket),
        Ok(Retirement::Pending)
    );
    controller.write(CONTEXT, 1 << 59);
    assert_eq!(
        controller.observe_retirement(&ticket),
        Ok(Retirement::Pending)
    );
    assert_eq!(controller.storage.bindings[0], Some(original));

    controller.write(controller.iotlb, 0);
    assert_eq!(
        controller.observe_retirement(&ticket),
        Err(Error::Unconfirmed)
    );
    let recovered = controller.pending_retirement(original).unwrap();
    assert_eq!(
        controller.observe_retirement(&recovered),
        Err(Error::Unconfirmed)
    );
    assert_eq!(controller.storage.bindings[0], Some(original));
    assert_eq!(controller.storage.contexts.0.0[0x30], [0; 2]);
}

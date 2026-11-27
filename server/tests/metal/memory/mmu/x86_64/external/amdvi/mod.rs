use super::super::test_firmware::Firmware;
use super::{
    Controller, Error, FEATURES, RUNNING, STATUS, Storage,
    binding::{
        Admissions, Binding, Context, Scope, admit_binding, binding_slot, reserve_binding_numbers,
    },
    directory::{DENY, Table},
    queue::{Completion, Progress, Queue},
};
use crate::metal::memory::{
    access::treaty::device::DeviceWindow,
    mmu::treaty::{Retirement, Unbind},
};
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress};
use std::boxed::Box;

fn context(requester: u16, domain: u16) -> Context {
    Context {
        requester,
        domain,
        root: PhysicalAddress::new(0x1000),
        levels: 1,
    }
}

fn scope(segment: u16) -> Scope {
    Scope {
        registers: PhysicalAddress::new(0xfed9_0000)
            .range(ByteLength::new(0x4000))
            .unwrap(),
        segment,
        device: 0,
    }
}

fn binding(number: u64, context: Context) -> Binding {
    Binding {
        scope: scope(0),
        epoch: 1,
        number,
        retirement: number + 1,
        context,
    }
}

#[test]
fn final_binding_has_an_exact_retirement_stamp_after_sequence_exhaustion() {
    let mut next = u64::MAX - 3;
    assert_eq!(
        reserve_binding_numbers(&mut next),
        Ok((u64::MAX - 2, u64::MAX - 1))
    );
    assert_eq!(reserve_binding_numbers(&mut next), Err(Error::Exhausted));
    assert_eq!(next, u64::MAX - 1);

    let mut next = u64::MAX - 2;
    let (number, retirement) = reserve_binding_numbers(&mut next).unwrap();
    let original = binding(number, context(0x30, 1));
    assert_eq!(reserve_binding_numbers(&mut next), Err(Error::Exhausted));
    assert_eq!(original.retirement, retirement);
    assert_eq!(retirement, u64::MAX);
    assert_eq!(binding_slot(&[Some(original)], original), Ok(0));
}

#[test]
fn multiple_requesters_hold_independent_slots_and_domains() {
    let first = binding(2, context(0x30, 1));
    let second = binding(3, context(0x38, 2));
    assert_eq!(admit_binding(&[Some(first), None], second.context()), Ok(1));
    assert_eq!(
        admit_binding(&[Some(first), Some(second)], context(0x40, 3)),
        Err(Error::Exhausted)
    );
    assert_eq!(
        admit_binding(&[Some(first), None], context(0x30, 3)),
        Err(Error::Occupied)
    );
    assert_eq!(
        admit_binding(&[None, Some(second)], context(0x40, 2)),
        Err(Error::Occupied)
    );
    assert_eq!(
        admit_binding(&[None, Some(second)], context(0x30, 1)),
        Ok(0)
    );
}

#[test]
fn a_rebound_requester_rejects_the_previous_generation_and_epoch() {
    let original = binding(2, context(0x30, 1));
    let rebound = binding(3, original.context());
    let mut admissions = Admissions {
        scope: original.scope,
        epoch: 0,
    };
    admissions.advance(original.scope).unwrap();
    assert_eq!(admissions.epoch, original.epoch);
    admissions.advance(original.scope).unwrap();
    let readmitted = Binding {
        epoch: admissions.epoch,
        ..original
    };
    assert_eq!(binding_slot(&[Some(rebound)], original), Err(Error::Stale));
    assert_eq!(
        binding_slot(&[Some(readmitted)], original),
        Err(Error::Stale)
    );
    assert_eq!(binding_slot(&[Some(readmitted)], readmitted), Ok(0));
}

#[test]
fn admission_identity_exhaustion_cannot_wrap_into_an_old_epoch() {
    let original = scope(0);
    let mut admissions = Admissions {
        scope: original,
        epoch: u64::MAX - 1,
    };
    assert_eq!(admissions.advance(scope(1)), Err(Error::Window));
    assert_eq!(admissions.epoch, u64::MAX - 1);
    assert_eq!(admissions.advance(original), Ok(()));
    assert_eq!(admissions.epoch, u64::MAX);
    assert_eq!(admissions.advance(original), Err(Error::Exhausted));
    assert_eq!(admissions.epoch, u64::MAX);
}

#[test]
fn only_the_original_running_batch_can_release_the_binding() {
    let mut description = [0; 76];
    description[..4].copy_from_slice(b"IVRS");
    description[48..52].copy_from_slice(&[0x10, 0, 28, 0]);
    description[54] = 0x40;
    description[56..64].copy_from_slice(&0xfed8_0000u64.to_le_bytes());
    description[72..].copy_from_slice(&[2, 0x30, 0, 0]);
    let firmware = Firmware::new(description);
    let unit = firmware.acpi().ivhd().unwrap().unwrap();
    let mut registers = [0u64; 0x4000 / 8];
    registers[FEATURES / 8] = 1 << 6;
    let base = registers.as_mut_ptr().expose_provenance() as u64;
    let mut window =
        unsafe { DeviceWindow::admit(unit.registers(), InternalAddress::new(base)) }.unwrap();
    let mut devices = Box::<Table>::new_uninit();
    unsafe { devices.as_mut_ptr().write_bytes(0, 1) };
    let mut devices = unsafe { devices.assume_init() };
    devices.0.fill(DENY);
    let mut queue = Queue::EMPTY;
    let mut completion = Completion::zeroed();
    let mut slots = [None];
    let mut admissions = Admissions::new(unit);
    let storage = Storage {
        table: (&mut devices, PhysicalAddress::new(0x20_0000)),
        queue: (&mut queue, PhysicalAddress::new(0x40_0000)),
        completion: (&mut completion, PhysicalAddress::new(0x40_1000)),
    };
    let mut controller =
        unsafe { Controller::admit(unit, &mut window, storage, &mut slots, &mut admissions) }
            .unwrap();
    controller.progress = Progress::Ready;
    controller.write(STATUS, RUNNING);

    assert_eq!(
        unsafe { controller.bind(context(0x30, 1)) },
        Err(Error::Pending)
    );
    let original = controller.bindings[0].unwrap();
    let completion = controller.storage.completion.0.0.get();
    unsafe { completion.write_volatile(original.number) };
    assert_eq!(controller.poll(), Ok(()));
    assert_eq!(controller.binding(0x30), Some(original));

    let ticket = unsafe { controller.retire(original) }.unwrap();
    let other = Binding {
        epoch: original.epoch + 1,
        ..original
    };
    assert!(controller.pending_retirement(other).is_none());
    assert_eq!(controller.poll(), Err(Error::Occupied));
    assert_eq!(controller.storage.table.0.0[0x30], DENY);
    unsafe { completion.write_volatile(original.number) };
    assert_eq!(
        controller.observe_retirement(&ticket),
        Ok(Retirement::Pending)
    );
    assert_eq!(controller.bindings[0], Some(original));

    unsafe { completion.write_volatile(original.retirement) };
    controller.write(STATUS, 0);
    assert_eq!(
        controller.observe_retirement(&ticket),
        Err(Error::Unconfirmed)
    );
    let recovered = controller.pending_retirement(original).unwrap();
    assert_eq!(
        controller.observe_retirement(&recovered),
        Err(Error::Unconfirmed)
    );
    assert_eq!(controller.bindings[0], Some(original));
    assert_eq!(controller.storage.table.0.0[0x30], DENY);
}

use super::{Binding, Context, Error, admit_binding, binding_slot, reserve_epoch};
use core::sync::atomic::AtomicU64;
use limen_treaty::model::PhysicalAddress;

fn context(requester: u16, domain: u16) -> Context {
    Context {
        requester,
        domain,
        root: PhysicalAddress::new(0x1000),
        levels: 1,
    }
}

fn binding(number: u64, context: Context) -> Binding {
    Binding {
        unit: PhysicalAddress::new(0xfed9_0000),
        epoch: 1,
        number,
        context,
    }
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
    let readmitted = Binding {
        epoch: 2,
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
    let epochs = AtomicU64::new(u64::MAX - 1);
    assert_eq!(reserve_epoch(&epochs), Ok(u64::MAX - 1));
    assert_eq!(reserve_epoch(&epochs), Err(Error::Exhausted));
    assert_eq!(reserve_epoch(&epochs), Err(Error::Exhausted));
}

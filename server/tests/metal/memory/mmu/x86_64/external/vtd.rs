use super::{CommandId, Context, Error, Sequence};
use limen_treaty::model::PhysicalAddress;

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

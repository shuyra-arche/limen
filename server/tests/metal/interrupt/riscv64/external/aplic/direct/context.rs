use super::*;

#[test]
fn admission_requires_the_single_original_idc() {
    assert_eq!(Destination::new(9, 0, 1).verify_single(), Ok(()));
    assert_eq!(
        Destination::new(9, 0, 2).verify_single(),
        Err(Failure::Scope)
    );
    assert_eq!(
        Destination::new(9, 1, 2).verify_single(),
        Err(Failure::Scope)
    );
    assert_eq!(
        Destination::new(9, 1, 1).verify_single(),
        Err(Failure::Scope)
    );
}

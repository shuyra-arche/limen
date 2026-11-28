use super::DescriptionStorage;
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

#[test]
fn original_description_is_published_from_one_resident_claim() {
    static STORAGE: DescriptionStorage = DescriptionStorage::new();

    let range = PhysicalRange::new(PhysicalAddress::new(4096), PhysicalAddress::new(8192));
    let ((), descriptions) = STORAGE
        .retain(|descriptions| {
            descriptions.device_tree_physical = range;
            Some(())
        })
        .unwrap();

    assert_eq!(descriptions.device_tree_physical, range);
    assert!(
        STORAGE
            .retain::<()>(|_| panic!("duplicate prepare"))
            .is_none()
    );
}

#[test]
fn rejected_original_description_cannot_reopen_boot() {
    static STORAGE: DescriptionStorage = DescriptionStorage::new();

    assert!(STORAGE.retain::<()>(|_| None).is_none());
    assert!(
        STORAGE
            .retain::<()>(|_| panic!("prepare after failure"))
            .is_none()
    );
}

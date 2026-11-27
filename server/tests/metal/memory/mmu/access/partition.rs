use super::{DeviceMapping, DeviceWindow, Error, Extent};
use core::marker::PhantomData;
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress, PhysicalRange};

fn geometry(physical: PhysicalRange) -> Extent {
    Extent::checked(physical, InternalAddress::new(0x4000)).unwrap()
}

#[test]
fn device_subranges_inherit_the_virtual_displacement_and_are_disjoint() {
    let physical = PhysicalAddress::new(0x1000)
        .range(ByteLength::new(0x20))
        .unwrap();
    let mut window =
        unsafe { DeviceWindow::admit(physical, InternalAddress::new(0x4000)) }.unwrap();
    let middle = PhysicalAddress::new(0x1004)
        .range(ByteLength::new(12))
        .unwrap();
    let subrange = window.subrange(middle).unwrap();
    assert_eq!(subrange.physical(), middle);
    assert!(!subrange.supports_u64(0));
    assert!(subrange.supports_u64(4));
    assert!(!subrange.supports_u32(12));
    assert_eq!(
        window
            .subrange(
                PhysicalAddress::new(0x0fff)
                    .range(ByteLength::new(2))
                    .unwrap()
            )
            .err(),
        Some(Error::Range)
    );
    let (window, error) = window.split(ByteLength::new(0x20)).err().unwrap();
    assert_eq!(error, Error::Range);
    let (left, right) = window.split(ByteLength::new(16)).ok().unwrap();
    assert_eq!(left.physical().end(), right.physical().start());
    assert_eq!(right.physical().end(), physical.end());
    assert!(left.supports_u64(8) && right.supports_u64(8));
    assert!(!left.supports_u32(16) && !right.supports_u32(16));
}

#[test]
fn retained_mapping_returns_the_original_owner_after_child_access() {
    use core::cell::Cell;

    struct Retention<'a>(&'a Cell<bool>);
    impl Drop for Retention<'_> {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }

    let live = Cell::new(true);
    let physical = PhysicalAddress::new(0x1000)
        .range(ByteLength::new(16))
        .unwrap();
    let mut mapping = DeviceMapping {
        extent: geometry(physical),
        retention: Retention(&live),
        local: PhantomData,
    };
    let view = mapping.window();
    let (left, right) = view.split(ByteLength::new(8)).ok().unwrap();
    assert!(left.supports_u64(0) && right.supports_u64(0) && live.get());
    let original = mapping.into_retention();
    assert!(live.get());
    drop(original);
    assert!(!live.get());
}

use kernel::metal::memory::mmu::{
    treaty::{
        LeafAccess::{ReadExecute, ReadOnly, ReadWrite},
        Memory::{Device, Normal},
        Privilege::{Kernel, User},
    },
    x86_64::internal::context::Controls,
};
use limen_treaty::model::{InternalAddress, InternalRange, PhysicalAddress};

fn controls() -> Controls {
    Controls {
        cr0: (1 << 16) | (1 << 31),
        cr3: 0x2000,
        cr4: 1 << 5,
        efer: 3 << 10,
        pat: Some(6u64 << 32), // WB first occurs at PAT index four.
    }
}

#[test]
fn rejects_uncovered_control_modes_and_width() {
    let active = controls();
    assert!(
        Controls {
            pat: None,
            ..active
        }
        .format(48, true)
        .is_none()
    );
    for bit in [7, 12, 17, 22, 23, 24] {
        assert!(
            Controls {
                cr4: active.cr4 | 1 << bit,
                ..active
            }
            .format(48, true)
            .is_none()
        );
    }
    assert!(
        Controls {
            cr0: 1 << 31,
            ..active
        }
        .format(48, true)
        .is_none()
    );
    assert!(
        Controls {
            efer: 1 << 10,
            ..active
        }
        .format(48, true)
        .is_none()
    );
    assert!(
        Controls {
            cr3: active.cr3 | (1 << 3),
            ..active
        }
        .format(48, true)
        .is_none()
    );
    assert!(
        Controls {
            cr3: 1usize << 48,
            ..active
        }
        .format(48, true)
        .is_none()
    );
}

#[test]
fn qualified_leaf_sizes_pat_bits_and_authority() {
    let format = controls().format(48, true).unwrap();
    let zero = PhysicalAddress::new(0);
    assert_eq!(format.index(InternalAddress::new(1 << 47), 3), None);
    assert_eq!(
        format.index(InternalAddress::new(0xffff_8000_0000_0000), 3),
        Some(256)
    );
    assert_eq!(format.branch(zero, User), Some(7));
    assert_eq!(format.branch(PhysicalAddress::new(1 << 48), Kernel), None);
    assert_eq!(format.leaf(zero, 0, User, ReadExecute, Normal), Some(0x85));
    assert_eq!(
        format.leaf(zero, 1, Kernel, ReadWrite, Normal),
        Some((1 << 63) | 0x1083)
    );
    assert_eq!(
        format.leaf(zero, 2, Kernel, ReadOnly, Device),
        Some((1 << 63) | 0x81)
    );
    assert_eq!(format.leaf(zero, 1, Kernel, ReadExecute, Device), None);
    assert_eq!(
        format.leaf(PhysicalAddress::new(4096), 1, User, ReadOnly, Normal),
        None
    );
    assert_eq!(
        controls()
            .format(48, false)
            .unwrap()
            .leaf(zero, 2, User, ReadOnly, Normal),
        None
    );
    assert_eq!(format.leaf(zero, 3, User, ReadOnly, Normal), None);
}

#[test]
fn range_rejects_a_canonical_hole_between_endpoints() {
    let range = |first, end| {
        InternalRange::new(InternalAddress::new(first), InternalAddress::new(end)).unwrap()
    };
    let format = controls().format(48, true).unwrap();
    assert!(format.is_canonical_range(range(0, 1 << 47)));
    assert!(!format.is_canonical_range(range((1 << 47) - 1, 0xffff_8000_0000_0001)));
}

#[test]
fn native_non_leaf_retains_only_the_original_branch() {
    let format = controls().format(48, true).unwrap();
    let physical = PhysicalAddress::new(0x3000);
    let encoded = format.branch(physical, Kernel).unwrap();
    assert!(format.matches_branch(physical, encoded));
    assert!(format.matches_branch(physical, encoded | (1 << 5)));
    assert!(!format.matches_branch(physical, encoded | (1 << 6)));
    assert!(!format.matches_branch(physical, encoded | 4));
    assert!(!format.matches_branch(PhysicalAddress::new(0x4000), encoded));
    assert_eq!(
        format.inherited_branch(encoded | 4 | (1 << 5)),
        Some(encoded | (1 << 5))
    );
    assert_eq!(format.inherited_branch(encoded | (1 << 7)), None);
    assert_eq!(format.inherited_branch(encoded & !1), None);
    assert_eq!(format.inherited_branch(encoded | (1 << 48)), None);
    assert_eq!(format.inherited_branch(3), None);
}

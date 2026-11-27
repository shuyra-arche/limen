use kernel::metal::memory::mmu::internal::{
    LeafAccess::{ReadExecute, ReadOnly, ReadWrite},
    Memory::{Device, Normal},
    Privilege::{Kernel, User},
    aarch64::{Controls, Root},
};
use limen_treaty::model::{InternalAddress, InternalRange, PhysicalAddress};

fn controls() -> Controls {
    Controls {
        mair: 0xff00,
        tcr: 0x0000_0005_b510_3510,
        ttbr0: 0x1000,
        ttbr1: 0x2000,
        sctlr: 1,
    }
}

#[test]
fn rejects_extensions_disabled_walks_and_unsupported_widths() {
    let active = controls();
    for bit in [23, 37, 38, 59] {
        assert!(
            Controls {
                tcr: active.tcr | 1 << bit,
                ..active
            }
            .format(Root::Upper, 5)
            .is_none()
        );
    }
    assert!(active.format(Root::Upper, 4).is_none());
    assert!(active.format(Root::Upper, (15 << 28) | 5).is_none());
    assert!(
        Controls { sctlr: 0, ..active }
            .format(Root::Upper, 5)
            .is_none()
    );
}

#[test]
fn distinct_upper_root_and_exact_descriptor_rights() {
    let format = controls().format(Root::Upper, 5).unwrap();
    let zero = PhysicalAddress::new(0);
    assert_eq!(
        format.index(InternalAddress::new(0xffff_0000_0000_0000), 3),
        Some(0)
    );
    assert_eq!(format.index(InternalAddress::new(1 << 48), 3), None);
    assert_eq!(format.branch(PhysicalAddress::new(4096)), Some(0x1003));
    assert_eq!(format.branch(PhysicalAddress::new(1 << 48)), None);
    let user_rx = format.leaf(zero, 0, User, ReadExecute, Normal).unwrap();
    assert_eq!(user_rx, (1 << 53) | 0xfc7);
    let kernel_rw = format.leaf(zero, 1, Kernel, ReadWrite, Normal).unwrap();
    assert_eq!(kernel_rw, (3 << 53) | 0x705);
    assert_eq!(
        format.leaf(zero, 2, User, ReadOnly, Device),
        Some((3 << 53) | 0xec1)
    );
    assert_eq!(format.leaf(zero, 0, Kernel, ReadExecute, Device), None);
    assert_eq!(
        format.leaf(PhysicalAddress::new(4096), 1, User, ReadOnly, Normal),
        None
    );
    assert_eq!(format.leaf(zero, 3, Kernel, ReadOnly, Normal), None);
}

#[test]
fn range_rejects_a_canonical_hole_between_endpoints() {
    let range = |first, end| {
        InternalRange::new(InternalAddress::new(first), InternalAddress::new(end)).unwrap()
    };
    let lower = controls().format(Root::Lower, 5).unwrap();
    let upper = controls().format(Root::Upper, 5).unwrap();
    let high = 0xffff_0000_0000_0000;
    assert!(lower.is_canonical_range(range(0, 1 << 48)));
    assert!(upper.is_canonical_range(range(high, high + 1)));
    assert!(!lower.is_canonical_range(range(0, high + 1)));
    assert!(!upper.is_canonical_range(range(0, high + 1)));
}

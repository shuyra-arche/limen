use kernel::metal::mmu::internal::riscv::{LeafAccess, Mode, context::Context};
use limen_treaty::model::{InternalAddress, InternalRange, PageSize, PhysicalAddress};

#[test]
fn indexes_both_canonical_ranges_at_every_supported_level() {
    let sizes = [1 << 12, 1 << 21, 1 << 30, 1 << 39, 1 << 48];
    for (mode, bits, levels) in [
        (Mode::Sv39, 39, 3),
        (Mode::Sv48, 48, 4),
        (Mode::Sv57, 57, 5),
    ] {
        let half = 1_u64 << (bits - 1);
        let high = u64::MAX - (half - 1);
        assert_eq!(mode.virtual_bits(), bits);
        assert_eq!(mode.levels(), levels);
        for (level, &size) in sizes[..levels].iter().enumerate() {
            assert_eq!(mode.leaf_size(level), PageSize::new(size));
            assert_eq!(
                mode.index(InternalAddress::new(half - 1), level),
                Some(if level == levels - 1 { 255 } else { 511 })
            );
            assert_eq!(
                mode.index(InternalAddress::new(high), level),
                Some(if level == levels - 1 { 256 } else { 0 })
            );
            assert_eq!(mode.index(InternalAddress::new(u64::MAX), level), Some(511));
            assert_eq!(mode.index(InternalAddress::new(half), level), None);
            assert_eq!(mode.index(InternalAddress::new(high - 1), level), None);
        }
        assert_eq!(mode.leaf_size(levels), None);
        assert_eq!(mode.index(InternalAddress::new(0), levels), None);
    }
}

#[test]
fn encodes_aligned_leaves_and_rejects_unrepresentable_inputs() {
    let width_end = PhysicalAddress::new(1 << 56);
    let physical = PhysicalAddress::new((1 << 56) - (1 << 12));
    let unaligned = PhysicalAddress::new(physical.get() + 1);
    let permissions = [
        (LeafAccess::ReadOnly, 0x43),
        (LeafAccess::ReadWrite, 0xc7),
        (LeafAccess::ReadExecute, 0x4b),
    ];
    assert_eq!(Mode::branch(physical), Some((physical.get() >> 2) | 1));
    assert_eq!(Mode::branch(width_end), None);
    assert_eq!(Mode::branch(unaligned), None);

    for (mode, satp) in [(Mode::Sv39, 8), (Mode::Sv48, 9), (Mode::Sv57, 10)] {
        let context = Context::paged(mode, physical).unwrap();
        assert_eq!(context.bits(), (satp << 60) | physical.get() >> 12);
        assert_eq!(Context::from_bits(context.bits()), Some(context));
        assert_eq!(context.mode(), Some(mode));
        assert_eq!(context.root(), Some(physical));
        assert_eq!(Context::paged(mode, width_end), None);
        assert_eq!(Context::paged(mode, unaligned), None);
        assert_eq!(Context::from_bits(context.bits() | (1 << 44)), None);
        for level in 0..mode.levels() {
            let size = mode.leaf_size(level).unwrap().get();
            for (permission, flags) in permissions {
                let address = PhysicalAddress::new(size);
                assert_eq!(
                    mode.leaf(address, level, permission),
                    Some((size >> 2) | flags)
                );
                assert_eq!(
                    mode.leaf(PhysicalAddress::new(size / 2), level, permission),
                    None
                );
                assert_eq!(mode.leaf(width_end, level, permission), None);
            }
        }
        assert_eq!(
            mode.leaf(physical, mode.levels(), LeafAccess::ReadOnly),
            None
        );
    }

    assert_eq!(Context::from_bits(0), Some(Context::BARE));
    assert_eq!(Context::BARE.root(), None);
    assert_eq!(Context::from_bits(1), None);
    assert_eq!(Context::from_bits(11 << 60), None);
}

#[test]
fn canonical_ranges_check_occupied_bytes_and_reject_the_hole() {
    for mode in [Mode::Sv39, Mode::Sv48, Mode::Sv57] {
        let half = 1_u64 << (mode.virtual_bits() - 1);
        let high = u64::MAX - (half - 1);
        let range = |first, end| {
            InternalRange::new(InternalAddress::new(first), InternalAddress::new(end)).unwrap()
        };
        assert!(mode.is_canonical_range(range(0, half)));
        assert!(mode.is_canonical_range(range(high, u64::MAX)));
        assert!(!mode.is_canonical_range(range(0, half + 1)));
        assert!(!mode.is_canonical_range(range(half, high)));
        assert!(!mode.is_canonical_range(range(half - 1, high + 1)));
        assert!(!mode.is_canonical_range(range(high - 1, high + 1)));
    }
}

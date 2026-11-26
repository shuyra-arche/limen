use limen_treaty::model::{
    Alignment, ByteLength, ByteOffset, ExternalAddress, InternalAddress, InternalRange, PageCount,
    PageOffset, PageSize, PhysicalAddress,
};

#[test]
fn coordinates_use_checked_arithmetic_and_keep_their_units() {
    let zero = InternalAddress::new(0);
    let last = InternalAddress::new(u64::MAX);
    let one_byte = ByteLength::new(1);
    assert_eq!(zero.advance(ByteLength::new(0)), Some(zero));
    assert_eq!(last.distance(zero), Some(ByteLength::new(u64::MAX)));
    assert_eq!(zero.distance(last), None);
    assert_eq!(last.advance(one_byte), None);
    assert_eq!(zero.retreat(one_byte), None);

    let external = ExternalAddress::new(5);
    assert_eq!(external.advance(one_byte), Some(ExternalAddress::new(6)));
    let offset = ByteOffset::new(5);
    assert_eq!(offset.retreat(one_byte), Some(ByteOffset::new(4)));
    let page = PageOffset::new(5);
    assert_eq!(page.advance(PageCount::new(1)), Some(PageOffset::new(6)));
    assert_eq!(page.distance(PageOffset::new(3)), Some(PageCount::new(2)));
}

#[test]
fn lengths_and_counts_report_overflow() {
    let max = ByteLength::new(u64::MAX);
    let one = ByteLength::new(1);
    assert_eq!(max.checked_add(one), None);
    assert_eq!(ByteLength::new(0).checked_sub(one), None);
    assert_eq!(max.checked_mul(2), None);
    assert_eq!(max.checked_mul(0), Some(ByteLength::new(0)));
    assert_eq!(max.as_usize(), usize::try_from(u64::MAX).ok());

    let max = PageCount::new(u64::MAX);
    let one = PageCount::new(1);
    assert_eq!(max.checked_add(one), None);
    assert_eq!(PageCount::new(0).checked_sub(one), None);
    assert_eq!(max.checked_mul(2), None);
    assert_eq!(max.as_usize(), usize::try_from(u64::MAX).ok());
}

#[test]
fn range_rejects_empty_reversed_and_overflowing_extents() {
    let first = InternalAddress::new(0);
    let last = InternalAddress::new(u64::MAX);
    assert_eq!(InternalRange::new(first, first), None);
    assert_eq!(InternalRange::new(last, first), None);
    assert_eq!(first.range(ByteLength::new(0)), None);
    assert_eq!(last.range(ByteLength::new(1)), None);
    assert_eq!(first.range(ByteLength::new(u64::MAX)).unwrap().end(), last);
}

#[test]
fn range_geometry_is_half_open() {
    let point = InternalAddress::new;
    let left = InternalRange::new(point(0), point(3)).unwrap();
    let right = InternalRange::new(point(3), point(7)).unwrap();
    let whole = InternalRange::new(point(0), point(7)).unwrap();
    let overlap = InternalRange::new(point(2), point(5)).unwrap();
    let gap = InternalRange::new(point(8), point(9)).unwrap();

    assert!(left.contains(point(0)));
    assert!(!left.contains(point(3)));
    assert!(whole.covers(left));
    assert!(!left.covers(whole));
    assert!(left.adjacent(right));
    assert!(!left.overlaps(right));
    assert_eq!(left.intersect(right), None);
    assert!(left.overlaps(overlap));
    assert_eq!(
        left.intersect(overlap),
        InternalRange::new(point(2), point(3))
    );
    assert_eq!(left.merge(right), Some(whole));
    assert_eq!(left.merge(gap), None);
    assert_eq!(whole.split(point(3)), Some((left, right)));
    assert_eq!(whole.split(whole.start()), None);
    assert_eq!(whole.split(whole.end()), None);
}

#[test]
fn page_conversion_and_alignment_handle_numeric_limits() {
    assert_eq!(PageSize::new(0), None);
    assert_eq!(PageSize::new(3), None);
    assert_eq!(Alignment::new(0), None);
    assert_eq!(Alignment::new(3), None);

    let page = PageSize::new(4096).unwrap();
    assert_eq!(page.covering(ByteLength::new(0)), PageCount::new(0));
    assert_eq!(page.exact(ByteLength::new(0)), Some(PageCount::new(0)));
    assert_eq!(page.covering(ByteLength::new(4097)), PageCount::new(2));
    assert_eq!(page.exact(ByteLength::new(4097)), None);
    assert_eq!(page.exact(ByteLength::new(8192)), Some(PageCount::new(2)));
    assert_eq!(page.bytes(PageCount::new(2)), Some(ByteLength::new(8192)));
    let covering = page.covering(ByteLength::new(u64::MAX));
    assert_eq!(covering, PageCount::new(1 << 52));
    assert_eq!(page.bytes(covering), None);

    let alignment = Alignment::new(4096).unwrap();
    let first = InternalAddress::new(0);
    let address = InternalAddress::new(4097);
    assert!(first.is_aligned(alignment));
    assert!(!address.is_aligned(alignment));
    assert_eq!(address.align_down(alignment), InternalAddress::new(4096));
    assert_eq!(
        address.align_up(alignment),
        Some(InternalAddress::new(8192))
    );
    assert_eq!(InternalAddress::new(u64::MAX).align_up(alignment), None);
}

#[test]
fn coordinates_share_geometry_without_marker_traits() {
    use limen_treaty::model::{ByteCoordinate, PageRange, PhysicalRange};

    enum Local {}
    let start = ByteCoordinate::<Local>::new(5);
    let range = start.range(ByteLength::new(7)).unwrap();
    assert_eq!(core::mem::size_of_val(&start), 8);
    assert_eq!(range.length(), ByteLength::new(7));
    assert_eq!(range.end().retreat(range.length()), Some(start));
    assert_eq!(range.start(), start);
    assert!(range.start() < range.end());
    let mut index = std::collections::HashMap::new();
    index.insert(start, range);
    assert_eq!(index.get(&start), Some(&range));

    let physical = PhysicalAddress::new(4096);
    let range = PhysicalRange::new(physical, PhysicalAddress::new(8192)).unwrap();
    assert_eq!(range.length(), ByteLength::new(4096));
    assert_eq!(
        PhysicalAddress::new(u64::MAX).range(ByteLength::new(1)),
        None
    );

    let pages = PageRange::new(PageOffset::new(2), PageOffset::new(7)).unwrap();
    assert_eq!(pages.length(), PageCount::new(5));
    assert_eq!(pages.start().range(pages.length()), Some(pages));
}

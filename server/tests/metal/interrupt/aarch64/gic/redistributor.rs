use super::{
    FRAME_BYTES, FRAME_LIMIT, Failure, FrameHandoff, LAST, Slot, TYPER, VIRTUAL_LPI, scan,
};
use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

fn region(bytes: usize) -> PhysicalRange {
    PhysicalAddress::new(0x080a_0000)
        .range(ByteLength::new(bytes as u64))
        .unwrap()
}

#[test]
fn complete_frames_share_a_domain_without_overlapping() {
    let span = region(3 * FRAME_BYTES);
    let first = (0xab00_0100u64 << 32) | VIRTUAL_LPI;
    let second = (0x0000_0001u64 << 32) | LAST;
    let mut slots = [const { Slot::EMPTY }; 2];
    let count = scan(span, None, &mut slots, |offset| match offset {
        TYPER => Ok(first),
        offset if offset == 2 * FRAME_BYTES + TYPER => Ok(second),
        _ => panic!("unexpected frame offset {offset}"),
    })
    .unwrap();
    assert_eq!(count, 2);
    let first = slots[0].0.unwrap();
    let second = slots[1].0.unwrap();
    assert_eq!(first.affinity, 0xab_0000_0100);
    assert_eq!(second.affinity, 1);
    assert_eq!(first.range.length().get(), 2 * FRAME_BYTES as u64);
    assert_eq!(second.range.length().get(), FRAME_BYTES as u64);
    assert!(first.range.adjacent(second.range));
    assert!(!first.range.overlaps(second.range));
    assert_eq!(first.range.merge(second.range), Some(span));
}

#[test]
fn scan_rejects_duplicate_affinity_truncated_frame_and_overlapping_stride() {
    let mut slots = [const { Slot::EMPTY }; 2];
    assert!(matches!(
        scan(region(2 * FRAME_BYTES), None, &mut slots, |offset| {
            Ok(if offset == TYPER { 0 } else { LAST })
        }),
        Err(Failure::Duplicate)
    ));
    let mut reads = 0;
    assert!(matches!(
        scan(region(FRAME_BYTES), None, &mut slots, |_| {
            reads += 1;
            Ok(0)
        }),
        Err(Failure::Range)
    ));
    assert_eq!(reads, 1);
    assert!(matches!(
        scan(
            region(2 * FRAME_BYTES),
            Some(FRAME_BYTES),
            &mut slots,
            |_| { Ok(VIRTUAL_LPI | LAST) }
        ),
        Err(Failure::Range)
    ));
}

#[test]
fn scan_has_a_finite_bound_when_firmware_omits_the_last_frame() {
    let mut reads = 0;
    let mut slots = [const { Slot::EMPTY }; FRAME_LIMIT];
    assert!(matches!(
        scan(
            region((FRAME_LIMIT + 1) * FRAME_BYTES),
            None,
            &mut slots,
            |_| {
                reads += 1;
                Ok((reads as u64) << 32)
            }
        ),
        Err(Failure::Capacity)
    ));
    assert_eq!(reads, FRAME_LIMIT);
}

#[test]
fn the_exact_frame_handoff_can_cross_a_processor_without_a_virtual_address() {
    fn send<T: Send>() {}
    send::<FrameHandoff<'static>>();
}

#![no_std]

use core::alloc::Layout;

#[path = "../../../src/metal/allocator/bump.rs"]
mod bump;

use bump::Bump;

#[test]
fn reserves_aligned_disjoint_ranges_inside_the_owned_interval() {
    let mut allocator = Bump::new(0x1008..0x3100).unwrap();
    let small = Layout::from_size_align(0x40, 0x40).unwrap();
    let page = Layout::from_size_align(0x1000, 0x1000).unwrap();
    let tail = Layout::from_size_align(0x100, 1).unwrap();

    assert_eq!(allocator.reserve(small), Some(0x1040..0x1080));
    assert_eq!(allocator.reserve(page), Some(0x2000..0x3000));
    assert_eq!(allocator.reserve(tail), Some(0x3000..0x3100));
    assert_eq!(allocator.reserve(small), None);
}

#[test]
fn a_request_that_exceeds_the_edge_leaves_all_remaining_bytes_available() {
    let base = 0x1010;
    let edge = 0x1030;
    let mut allocator = Bump::new(base..edge).unwrap();
    let too_large_after_alignment = Layout::from_size_align(0x20, 0x20).unwrap();
    let exact = Layout::from_size_align(edge - base, 1).unwrap();

    assert_eq!(allocator.reserve(too_large_after_alignment), None);
    assert_eq!(allocator.reserve(exact), Some(base..edge));
}

#[test]
fn rejects_address_overflow_and_empty_input() {
    let edge = usize::MAX;
    let base = edge - 15;
    let mut allocator = Bump::new(base..edge).unwrap();
    let bad_alignment = Layout::from_size_align(1, 32).unwrap();
    let bad_size = Layout::from_size_align(16, 1).unwrap();
    let exact = Layout::from_size_align(15, 1).unwrap();

    assert_eq!(allocator.reserve(bad_alignment), None);
    assert_eq!(allocator.reserve(bad_size), None);
    assert_eq!(allocator.reserve(exact), Some(base..edge));
    assert!(Bump::new(edge..edge).is_none());
}

mod buddy;

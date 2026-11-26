#[repr(align(8))]
struct Header([u32; 10]);

fn admitted(structure_size: u32, strings: u32, strings_size: u32, reservations: u32) -> bool {
    let words = [
        0xd00d_feed,
        128,
        56,
        strings,
        reservations,
        17,
        16,
        0,
        strings_size,
        structure_size,
    ];
    let header = Header(words.map(u32::to_be));
    // The parser reads only this aligned, initialized header.
    unsafe { super::extent(&raw const header.0, 0..0).is_some() }
}

#[test]
fn admits_adjacent_blocks_and_an_empty_strings_block() {
    assert!(admitted(16, 72, 16, 40));
    assert!(admitted(16, 72, 0, 40));
}

#[test]
fn rejects_impossible_structure_lengths_and_overlapping_blocks() {
    assert!(!admitted(0, 72, 16, 40));
    assert!(!admitted(12, 72, 16, 40));
    assert!(!admitted(17, 80, 8, 40));
    assert!(!admitted(32, 72, 8, 40));
    assert!(!admitted(16, 80, 8, 48));
    assert!(!admitted(16, 48, 8, 40));
}

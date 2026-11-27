use super::*;

#[test]
fn identical_legacy_and_extended_descriptions_are_one_unit() {
    let mut table = [0u8; 48 + 28 + 44];
    for (offset, kind, length) in [(48, 0x10, 28), (76, 0x11, 44)] {
        table[offset] = kind;
        table[offset + 2] = length;
        table[offset + 6] = 0x40;
        table[offset + 8..offset + 16].copy_from_slice(&0xfed8_0000u64.to_le_bytes());
        let end = offset + usize::from(length);
        table[end - 4..end].copy_from_slice(&[2, 48, 0, 0]);
    }
    assert!(decode(&table).unwrap().selects(48));
    table[76 + 16] = 1;
    assert_eq!(decode(&table), Err(Error::DuplicateRecord));
    table[76] = 0x20;
    assert_eq!(decode(&table), Err(Error::UnsupportedRevision));
    table[48 + 2] = 0;
    assert_eq!(decode(&table), Err(Error::InvalidRecord));
}

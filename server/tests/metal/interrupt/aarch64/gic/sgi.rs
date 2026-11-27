use super::selection;

#[test]
fn sgi_routes_one_original_pe_without_range_selector_or_broadcast() {
    assert_eq!(selection(1, 12), Some(0x0c00_0002));
    assert_eq!(selection(0x5a_0034_1203, 7), Some(0x005a_0034_0712_0008));
    assert_eq!(selection(16, 1), None);
    assert_eq!(selection(1, 16), None);
    assert_eq!(selection(0x0100_0000, 1), None);
}

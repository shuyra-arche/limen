use super::*;

#[test]
fn exception_masks_level_and_stack_remain_distinct() {
    assert_eq!(IRQ_FIQ << 6, 0xc0);
    for level in 0..4 {
        assert_eq!(el1(level << 2), level == 1);
    }
    for mask in 0..16 {
        for stack in 0..2 {
            assert_eq!(masked_stack(mask << 6, stack), mask == 15 && stack == 1);
        }
    }
}

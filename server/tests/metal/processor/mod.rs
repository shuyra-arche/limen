//! Portable field interpretation through the ordinary production library.

use kernel::metal::processor::description::{
    aarch64::status as arm,
    riscv64::status::{Extension, Status},
    x86_64::status,
};

#[test]
fn native_fields_have_portable_value_contracts() {
    let status = Status::from_bits(1 << 13);
    assert!(status.masked());
    assert_eq!(status.floating(), Extension::Initial);
    assert_eq!(status.vector(), Extension::Off);
    assert!(status.additional_off());
    assert!(!status.extensions_off());
    assert!(Status::from_bits(0).extensions_off());
    assert!(arm::el1(4));
    assert!(!arm::el1(8));
    assert!(arm::masked_stack(0x3c0, 1));
    assert!(!arm::masked_stack(0x3c0, 0));
}

#[test]
fn x86_machine_status_preserves_privilege_masks_and_paging() {
    for privilege in 0..4 {
        assert_eq!(status::kernel_selector(0x28 | privilege), privilege == 0);
    }
    assert!(status::paging(1 << 31));
    assert!(!status::paging(1));
    assert!(status::interrupts_masked(2));
    assert!(!status::interrupts_masked(0x202));
}

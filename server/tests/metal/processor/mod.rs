//! Portable field interpretation through the ordinary production library.

use kernel::metal::processor::{
    execution::description::aarch64,
    hart::{
        aarch64::status as arm,
        riscv::status::{Extension, Status},
    },
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
    assert!(aarch64::base_simd(1 << 20));
    assert!(!aarch64::base_simd((1 << 20) | (1 << 16)));
}

//! Portable field interpretation through the ordinary production library.

use kernel::metal::processor::{
    execution::description::aarch64,
    hart::{
        aarch64::status as arm,
        riscv64::status::{Extension, Status},
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

#[test]
fn x86_cooperative_profile_checks_each_enabled_component() {
    use kernel::metal::processor::{
        execution::description::x86_64::{StateControl, base_xcr0},
        hart::x86_64::status,
    };

    let edx = 7 << 24;
    let fxsr = 1 << 9;
    let extended = fxsr | (1 << 18);
    assert_eq!(
        StateControl::from_registers(edx, 0, 0, fxsr),
        Some(StateControl::Legacy)
    );
    for bit in [24, 25, 26] {
        assert_eq!(
            StateControl::from_registers(edx ^ (1 << bit), 0, 0, fxsr),
            None
        );
    }
    for cr0 in [1 << 2, 1 << 3] {
        assert_eq!(StateControl::from_registers(edx, 0, cr0, fxsr), None);
    }
    assert_eq!(StateControl::from_registers(edx, 0, 0, 0), None);
    for bits in 0..4 {
        let expected = (bits == 3).then_some(StateControl::Xcr0);
        assert_eq!(
            StateControl::from_registers(edx, bits << 26, 0, extended),
            expected
        );
    }
    assert_eq!(StateControl::from_registers(edx, 3 << 26, 0, fxsr), None);
    assert!(base_xcr0(3));
    for bits in [0, 1, 2, 7, (1 << 32) | 3] {
        assert!(!base_xcr0(bits));
    }
    for privilege in 0..4 {
        assert_eq!(status::kernel_selector(0x28 | privilege), privilege == 0);
    }
    assert!(status::paging(1 << 31));
    assert!(!status::paging(1));
    assert!(status::interrupts_masked(2));
    assert!(!status::interrupts_masked(0x202));
}

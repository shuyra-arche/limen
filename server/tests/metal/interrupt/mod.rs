//! Compile the actual controller admission boundary without native authority.

use kernel::metal::interrupt::{riscv64::external::aplic, x86_64::internal::apic};

#[test]
fn controller_admission_exists_in_the_ordinary_library() {
    // Resolve the real production operations, not a test-only copy or stub.
    // No native probe, mapping, or exclusive reception is fabricated or used.
    let _ = (apic::LocalApic::admit, aplic::Direct::admit);
}

#[test]
fn gic_system_register_feature_ignores_adjacent_fields() {
    use kernel::metal::interrupt::aarch64::gic::system_register_interface;

    for field in 0..16 {
        let bits = !(0xf << 24) | (field << 24);
        assert_eq!(system_register_interface(bits), matches!(field, 1 | 3));
    }
}

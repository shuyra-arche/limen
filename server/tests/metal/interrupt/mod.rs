//! Compile the actual controller admission boundary without native authority.

use kernel::metal::interrupt::{external::aplic, internal::apic};

#[test]
fn controller_admission_exists_in_the_ordinary_library() {
    // Resolve the real production operations, not a test-only copy or stub.
    // No native probe, mapping, or exclusive reception is fabricated or used.
    let _ = (apic::LocalApic::admit, aplic::Direct::admit);
}

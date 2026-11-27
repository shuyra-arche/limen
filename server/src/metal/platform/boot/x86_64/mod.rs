use crate::metal::{
    interrupt::x86_64::internal::apic,
    platform::boot::handoff::{Descriptions, Facts, uefi},
    processor::executor::x86_64::native,
};

mod major;
pub(crate) use major::major as enter_major;

/// Admit the loader facts and check the actual local APIC against MADT.
pub(crate) unsafe fn admit<'a>(
    processor: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Option<Facts<'a>> {
    let facts = unsafe { uefi::admit(processor, native::boot_identity(), input, descriptions) }?;
    if let Some(acpi) = descriptions.acpi.as_ref()
        && !apic::native::matches_madt(processor, acpi.madt()?)
    {
        return None;
    }
    Some(facts)
}

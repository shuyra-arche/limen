#[cfg(limen_target)]
use crate::metal::{
    boot::services::Services,
    firmware::facts::{Descriptions, Facts},
};

mod loader;
pub use loader::{AcpiInput, DeviceTree, Loader};

#[cfg(limen_target)]
pub(crate) unsafe fn admit<'a>(
    _hart: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Option<Facts<'a>> {
    // The trusted loader keeps this value and its buffers resident.
    let facts = unsafe { input.cast::<Loader<'a>>().as_ref() }?.admit(descriptions)?;
    #[cfg(target_arch = "x86_64")]
    if let Some(acpi) = descriptions.acpi.as_ref()
        && !crate::metal::hart::matches_madt(_hart, acpi.madt()?)
    {
        return None;
    }
    Some(facts)
}

#[cfg(limen_target)]
pub(crate) fn services_on_entry() -> &'static Services {
    Services::active()
}

#[cfg(limen_target)]
pub(crate) fn trap_services() -> &'static Services {
    Services::active()
}

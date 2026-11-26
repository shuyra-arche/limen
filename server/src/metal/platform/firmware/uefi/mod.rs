#[cfg(limen_firmware = "uefi")]
use crate::metal::{
    platform::boot::services::Services,
    platform::firmware::facts::{Descriptions, Facts},
};

mod loader;
pub use loader::{AcpiInput, DeviceTree, Loader};

#[cfg(limen_firmware = "uefi")]
pub(crate) unsafe fn admit<'a>(
    hart: usize,
    observed: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Option<Facts<'a>> {
    // The current loader handoff supplies an identity checked against the
    // physical CPU, rather than the trusted SBI entry's current-Hart ABI.
    if hart != observed {
        Services::active().reject(format_args!(
            "FAIL Limen boot: major CPU identity mismatch\n"
        ));
    }
    // The trusted loader keeps this value and its buffers resident.
    unsafe { input.cast::<Loader<'a>>().as_ref() }?.admit(descriptions)
}

#[cfg(limen_firmware = "uefi")]
pub(crate) fn services_on_entry() -> &'static Services {
    Services::active()
}

#[cfg(limen_firmware = "uefi")]
pub(crate) fn trap_services() -> &'static Services {
    Services::active()
}

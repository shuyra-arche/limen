use crate::{
    entry::boot::Boot,
    metal::{
        boot::{self, services::Services},
        firmware::{devicetree, map::FirmwareMap},
    },
};

mod loader;
pub use loader::Loader;

/// Hand off a loaded image after successful ExitBootServices.
///
/// # Safety
/// Run once on the major CPU in ring 0 with paging (x86) or non-secure EL1 (Arm).
/// Keep the image, identity mappings, loader stack and inputs reserved across delivery.
pub unsafe fn major(hart: usize, loader: &Loader<'_>, services: &'static Services) -> ! {
    services.install();
    unsafe { boot::enter_major(hart, loader) }
}

pub(crate) unsafe fn admit<'a>(hart: usize, input: *const ()) -> Option<Boot<'a>> {
    // The trusted loader keeps this value and its buffers resident.
    unsafe { input.cast::<Loader<'a>>().as_ref() }?.admit(hart)
}

pub(crate) fn services_on_entry() -> &'static Services {
    Services::active()
}

pub(crate) fn trap_services() -> &'static Services {
    Services::active()
}

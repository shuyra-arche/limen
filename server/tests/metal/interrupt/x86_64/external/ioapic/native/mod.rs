use kernel::entry::boot;
use uefi_fixture as fixture;

use fixture::q35;

mod scenario;
#[path = "../../../window.rs"]
mod window;

pub fn check(boot: boot::Boot<'_>) {
    fixture::check_handoff(&boot, None);
    let mut window = q35::local_window(&boot);
    unsafe { window::prepare(&mut window) };
    let mut controller = q35::local_controller(&boot, window);
    scenario::check(
        boot.acpi().unwrap().madt().unwrap(),
        boot.hart(),
        &mut controller,
    );
}

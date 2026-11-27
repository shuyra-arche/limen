use kernel::entry::boot;
use uefi_fixture as fixture;

#[path = "../../../../../../../target/support/pci/q35.rs"]
mod edu;

mod scenario;

pub fn check(boot: boot::Boot<'_>) {
    fixture::check_handoff(&boot, None);
    scenario::check_firmware(&boot);
}

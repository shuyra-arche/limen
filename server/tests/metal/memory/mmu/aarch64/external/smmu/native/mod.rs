use kernel::entry::boot;
use uefi_fixture as fixture;

mod scenario;

pub fn check(boot: boot::Boot<'_>) {
    fixture::check_handoff(&boot, None);
    scenario::check(&boot.device_tree_description().unwrap());
}

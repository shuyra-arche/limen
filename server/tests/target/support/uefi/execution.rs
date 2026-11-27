//! UEFI admission for the cross-Hart migration test.

use kernel::entry::boot::Boot;
use uefi::Status;
use uefi_fixture as fixture;

#[path = "../../../basis/execution/migration.rs"]
mod migration;

pub fn prepare() -> Result<(), Status> {
    #[cfg(target_arch = "x86_64")]
    fixture::q35::prepare_minor()?;
    Ok(())
}

pub fn check(boot: Boot<'_>, complete: fn() -> !) -> ! {
    let view = fixture::check_handoff(&boot, Some(&migration::MINOR));
    let target = fixture::target();
    let mut startup = fixture::inspect(&boot, &view);
    migration::run(
        boot,
        target,
        &mut startup,
        fixture::check_minor,
        |startup| fixture::inspect_started(startup, target),
        complete,
    )
}

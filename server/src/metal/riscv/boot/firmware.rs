use super::super::sbi;
use crate::entry::boot::Boot;

// OpenSBI transfers control to _start on one boot hart, with placement and
// readable ownership of the device tree guaranteed by the trusted platform.
#[unsafe(no_mangle)]
pub(super) extern "C" fn limen_boot(hart: usize, device_tree: usize) -> ! {
    crate::entry::boot::start(|| {
        let boot = unsafe { Boot::from_device_tree(hart, device_tree, super::image()) }
            .ok_or("invalid device tree header")?;
        boot.enter()
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn limen_boot_trap(cause: usize, pc: usize, value: usize, stack: usize) -> ! {
    sbi::services().reject(format_args!(
        "FAIL Limen boot trap: cause={cause:#x} pc={pc:#x} value={value:#x} stack={stack:#x}\n"
    ))
}

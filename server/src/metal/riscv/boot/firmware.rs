use super::super::sbi;
use crate::entry::boot::Boot;

// OpenSBI transfers control to _start on one boot hart, with placement and
// readable ownership of the device tree guaranteed by the trusted platform.
#[unsafe(no_mangle)]
extern "C" fn limen_boot(hart: usize, device_tree: usize) -> ! {
    let established = super::established();
    let firmware = sbi::extensions();
    if !established || !firmware.console || !firmware.reset {
        sbi::reject(
            firmware,
            format_args!("FAIL Limen boot: invalid supervisor or SBI handoff\n"),
        );
    }
    let Some(boot) = (unsafe { Boot::from_device_tree(hart, device_tree, super::image()) }) else {
        sbi::reject(
            firmware,
            format_args!("FAIL Limen boot: invalid device tree header\n"),
        );
    };
    boot.enter()
}

#[unsafe(no_mangle)]
extern "C" fn limen_boot_trap(cause: usize, pc: usize, value: usize, stack: usize) -> ! {
    sbi::reject(
        sbi::extensions(),
        format_args!(
            "FAIL Limen boot trap: cause={cause:#x} pc={pc:#x} value={value:#x} stack={stack:#x}\n"
        ),
    )
}

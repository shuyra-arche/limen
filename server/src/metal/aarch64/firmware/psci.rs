//! PSCI services using the HVC conduit selected by the virt board.
use core::arch::asm;

pub(in crate::metal::aarch64) unsafe fn cpu_on(
    hart: usize,
    entry: *const (),
    ptr: usize,
) -> Result<(), isize> {
    let status: i64;
    unsafe {
        asm!("dsb sy","hvc #0", inout("x0") 0xc4000003u64 => status,
        in("x1") hart,in("x2") entry,in("x3") ptr,
        clobber_abi("C"),options(nostack));
    }
    if status == 0 {
        Ok(())
    } else {
        Err(status as isize)
    }
}

pub(crate) fn reset(_: bool) {
    // PSCI SYSTEM_OFF has no success or failure reason field.
    unsafe {
        asm!("hvc #0", inout("x0") 0x8400_0008usize => _,
             clobber_abi("C"), options(nostack));
    }
}

//! Native RV64 admission must reject live control state without consuming material.

use core::arch::asm;
use kernel::{
    basis::{
        execution::{Error, EstablishError, EstablishFailure, Execution, KernelBinding},
        room::{Harts, Occupancy},
        unit::{Stacks, UnitRecord},
    },
    entry::boot::Boot,
    metal::processor::treaty::Dispatchers,
};

pub(super) fn reject_constructor(
    boot: &mut Boot<'_>,
    stacks: &'static mut Stacks,
    records: &'static mut [UnitRecord<KernelBinding>],
    harts: &'static mut Harts<1>,
    dispatchers: &'static mut Dispatchers<Occupancy, 1>,
) -> EstablishFailure<1> {
    let major = boot.hart();
    let device_tree = boot.device_tree();
    let stack_address = core::ptr::from_mut(&mut *stacks);
    let record_address = records.as_ptr();
    let hart_address = core::ptr::from_mut(&mut *harts);
    let dispatcher_address = core::ptr::from_mut(&mut *dispatchers);
    let admitted = boot.take_major().expect("original native major relation");
    assert!(boot.take_major().is_none());
    // Individual interrupt sources remain masked. SIE makes the execution
    // environment unsupported before the constructor binds the stack supply.
    unsafe { asm!("csrsi sstatus, 2", options(nostack)) };
    let rejected = Execution::new(
        admitted,
        boot.terminal(),
        stacks,
        records,
        harts,
        dispatchers,
    );
    unsafe { asm!("csrci sstatus, 2", options(nostack)) };
    let Err(failure) = rejected else {
        panic!("execution admitted with interrupts enabled");
    };
    assert_eq!(failure.reason, EstablishError::Unsupported);
    assert_eq!(boot.hart(), major);
    assert_eq!(boot.device_tree(), device_tree);
    assert!(core::ptr::eq(failure.stacks, stack_address));
    assert!(core::ptr::eq(failure.records.as_ptr(), record_address));
    assert!(core::ptr::eq(failure.harts, hart_address));
    assert!(core::ptr::eq(failure.dispatchers, dispatcher_address));
    failure
}

pub(super) fn reject_attach(execution: &Execution) {
    unsafe { asm!("csrsi sstatus, 2", options(nostack)) };
    assert_eq!(execution.attach(), Err(Error::Unsupported));
    unsafe { asm!("csrci sstatus, 2", options(nostack)) };
}

pub(super) fn reject_dispatch(execution: &'static Execution) {
    // The firmware handoff left every interrupt source masked. Change only
    // SIE to make the trusted entry reject this attempt before Occupy.
    unsafe { asm!("csrsi sstatus, 2", options(nostack)) };
    let rejected = execution.enter();
    unsafe { asm!("csrci sstatus, 2", options(nostack)) };
    assert_eq!(rejected, Err(Error::Unsupported));
    assert_eq!(execution.pending_leave(), Ok(None));

    // Change only the actual FS field: the integer-only kernel target
    // executes no FP instructions, so no extended state is consumed.
    let mut observed: usize;
    unsafe {
        asm!(
            "csrs sstatus, {initial}",
            "csrr {observed}, sstatus",
            initial = in(reg) 1usize << 13,
            observed = out(reg) observed,
            options(nostack),
        );
    }
    let rejected = execution.enter();
    unsafe { asm!("csrc sstatus, {}", in(reg) 3usize << 13, options(nostack)) };
    assert_ne!(observed & (3 << 13), 0, "QEMU exposes the actual FS field");
    assert_eq!(rejected, Err(Error::Unsupported));
    assert_eq!(execution.pending_leave(), Ok(None));

    // Global SIE is still off. A single locally enabled source is a
    // different admission failure and must not be mistaken for global SIE.
    unsafe {
        asm!(
            "csrs sie, {timer}",
            "csrr {observed}, sie",
            timer = in(reg) 1usize << 5,
            observed = out(reg) observed,
            options(nostack),
        );
    }
    let rejected = execution.enter();
    unsafe { asm!("csrc sie, {}", in(reg) 1usize << 5, options(nostack)) };
    assert_ne!(observed & (1 << 5), 0);
    assert_eq!(rejected, Err(Error::Unsupported));
    assert_eq!(execution.pending_leave(), Ok(None));
    kernel::metal::platform::boot::treaty::write("PASS native-control fs=1 source-mask=1 restored=1\n");
}

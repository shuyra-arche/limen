//! An IRQ resumes an occupied Kernel Unit before its ordinary cooperative leave.

use core::{
    arch::asm,
    cell::UnsafeCell,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed},
};
use kernel::{
    basis::execution::Error,
    entry::{boot::Boot, hook::Binding},
    metal::{
        chrono::riscv64::description::RiscvTimebase,
        interrupt::riscv64::internal::ipi::Receiver,
        processor::treaty::{RuntimeTrap, SoftwareInterrupt},
    },
};

struct Local {
    receiver: UnsafeCell<Option<Receiver>>,
    captures: AtomicUsize,
}

// This fixture publishes one original receiver to the current processor's
// handler. The ordinary side only borrows it with IRQ dispatch masked.
unsafe impl Sync for Local {}

static RECEPTION: Binding<Local> = Binding::new(
    Local {
        receiver: UnsafeCell::new(None),
        captures: AtomicUsize::new(0),
    },
    Some(interrupt),
    None,
);
static VECTORS: RuntimeTrap = RuntimeTrap::new(RECEPTION.entry());
static FREQUENCY: AtomicU32 = AtomicU32::new(0);

pub(super) fn install(boot: &Boot<'_>) {
    let timebase = RiscvTimebase::describe(boot.riscv_cpus()).unwrap().unwrap();
    FREQUENCY.store(timebase.hertz(), Relaxed);
    unsafe { VECTORS.install() }.unwrap();
}

fn interrupt(owner: &Local, capture: SoftwareInterrupt<'_>) {
    assert_eq!(super::execution().pending_leave(), Err(Error::Protection));
    let receiver = unsafe { (&mut *owner.receiver.get()).as_mut().unwrap() };
    receiver.acknowledge_interrupt(&capture).unwrap();
    owner.captures.fetch_add(1, Relaxed);
}

pub(super) fn receive(processor: usize) {
    let owner = RECEPTION.owner();
    assert_eq!(owner.captures.load(Relaxed), 0);
    let mut receiver = unsafe { Receiver::admit(processor) }.unwrap();
    receiver.open().unwrap();
    unsafe { owner.receiver.get().write(Some(receiver)) };
    let status: usize;
    let original: usize;
    let restored: usize;
    let interval = u64::from(FREQUENCY.load(Relaxed));
    unsafe {
        asm!(
            "mv {original}, sp",
            "rdtime t0",
            "add t0, t0, {interval}",
            "csrsi sip, 2",
            "csrsi sstatus, 2",
            "2: csrr t1, sip",
            "andi t1, t1, 2",
            "beqz t1, 3f",
            "rdtime t1",
            "bltu t1, t0, 2b",
            "3: csrr {status}, sstatus",
            "csrci sstatus, 2",
            "mv {restored}, sp",
            interval = in(reg) interval,
            status = out(reg) status,
            original = out(reg) original,
            restored = out(reg) restored,
            out("t0") _,
            out("t1") _,
            options(nostack),
        );
    }
    assert_ne!(status & 2, 0);
    assert_eq!(original, restored);
    assert_eq!(owner.captures.load(Relaxed), 1);
    assert_eq!(super::execution().pending_leave(), Ok(None));
    let mut receiver = unsafe { (&mut *owner.receiver.get()).take().unwrap() };
    assert_eq!(receiver.pending(), Ok(false));
    unsafe { receiver.shut() }.unwrap();
}

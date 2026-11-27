//! A fixed local xAPIC IRQ resumes the actual occupied Kernel Unit.

use core::{
    arch::asm,
    cell::RefCell,
    num::NonZeroU32,
    sync::atomic::{AtomicBool, Ordering::Relaxed},
};

use kernel::{
    basis::execution::Error,
    entry::{boot::Boot, hook::Binding},
    metal::{
        interrupt::{
            treaty::{Completion, HardwareClaim},
            x86_64::internal::apic::{Countdown, Divider, LocalApic, Reception, TimerState},
        },
        processor::treaty::{Irq, RuntimeTrap},
    },
};
use uefi_fixture::q35;

#[path = "../../../metal/interrupt/x86_64/prepare.rs"]
mod hardware;

const VECTOR: u8 = 0xf2;
const IF: usize = 1 << 9;
const REGISTER: usize = 0x1234_5678_9abc_def0;
const COUNTDOWN: Countdown = Countdown {
    ticks: NonZeroU32::new(10_000).unwrap(),
    divider: Divider::Sixteen,
};

static RECEPTION: Binding<Local> = Binding::new(
    Local {
        controller: RefCell::new(None),
        captured: AtomicBool::new(false),
    },
    interrupt,
);
static VECTORS: RuntimeTrap = RuntimeTrap::new(VECTOR, RECEPTION.entry());

struct Local {
    controller: RefCell<Option<LocalApic<'static>>>,
    captured: AtomicBool,
}

// Only the owning boot processor uses this controller. Ordinary accesses
// end before its non-nested IRQ can observe the same owner.
unsafe impl Sync for Local {}

impl Local {
    fn controller<T>(&self, action: impl FnOnce(&mut LocalApic<'static>) -> T) -> T {
        let mut controller = self.controller.borrow_mut();
        action(controller.as_mut().expect("original local APIC"))
    }
}

pub(super) fn install(boot: &Boot<'_>) {
    let mut window = q35::local_window(boot);
    unsafe { hardware::prepare(&mut window) };
    let mut controller = q35::local_controller(boot, window);
    unsafe { controller.retire_inherited_timer() }.unwrap();
    let vector = controller.reserve_vector(VECTOR).unwrap();
    controller.reserve_timer(vector).unwrap();
    assert!(
        RECEPTION
            .owner()
            .controller
            .replace(Some(controller))
            .is_none()
    );
    unsafe { VECTORS.install() }.unwrap();
}

fn interrupt(owner: &Local, mut irq: Irq<'_>) {
    assert_eq!(super::execution().pending_leave(), Err(Error::Protection));
    assert_eq!(irq.vector(), VECTOR);
    let processor = irq.processor().get();
    owner.controller(|controller| {
        assert_eq!(controller.timer_state(), Some(TimerState::Armed(COUNTDOWN)));
        let mut claim = unsafe { controller.receive_irq(&mut irq) }.unwrap();
        let identity = claim.identity();
        assert_eq!(identity.context(), processor as u64);
        assert_eq!(identity.source(), u32::from(VECTOR));
        assert_eq!(identity.round(), 1);
        assert_eq!(unsafe { claim.finish() }, Ok(Completion::Settled));
    });
    assert!(!owner.captured.swap(true, Relaxed));
}

pub(super) fn receive(processor: usize) {
    let owner = RECEPTION.owner();
    assert_eq!(q35::processor(), processor);
    assert!(!owner.captured.load(Relaxed));
    owner.controller(|controller| unsafe { controller.arm_timer(COUNTDOWN) }.unwrap());

    let start = q35::tsc();
    let interval = q35::tsc_frequency();
    while !owner.captured.load(Relaxed) {
        assert!(q35::tsc().wrapping_sub(start) < interval);
        let (before, enabled, after, flags, register): (usize, usize, usize, usize, usize);
        // The IRQ enters on its own IST. Probe the original stack, IF and
        // a caller-saved register across the real returning interrupt.
        unsafe {
            asm!(
                "mov {before}, rsp",
                "sti",
                "pushfq",
                "pop {enabled}",
                "cli",
                "mov {after}, rsp",
                "pushfq",
                "pop {flags}",
                before = out(reg) before,
                enabled = out(reg) enabled,
                after = out(reg) after,
                flags = out(reg) flags,
                inout("rax") REGISTER => register,
            );
        }
        assert_eq!(before, after);
        assert_ne!(enabled & IF, 0);
        assert_eq!(flags & IF, 0);
        assert_eq!(register, REGISTER);
    }
    assert_eq!(super::execution().pending_leave(), Ok(None));

    let mut controller = owner.controller.take().unwrap();
    assert_eq!(controller.reception(), Reception::Idle);
    controller.stop_timer().unwrap();
    let original = controller.release_timer().unwrap();
    assert_eq!(original.vector(), VECTOR);
    controller.release_vector(original).unwrap();
    assert_eq!(controller.timer_state(), None);
}

use core::{
    arch::{asm, x86_64::__cpuid},
    cell::RefCell,
    num::NonZeroU32,
    sync::atomic::{AtomicUsize, Ordering},
};
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::{
    entry::{boot, hook::Binding},
    metal::{
        chrono::x86_64::tsc::{Failure as TscFailure, Tsc},
        interrupt::{
            treaty::HardwareClaim,
            x86_64::internal::apic::{
                Countdown, Divider, Failure, LocalApic, Reception, TimerState,
            },
        },
        processor::treaty::{Irq, RuntimeTrap},
    },
};
use uefi_fixture::q35;

#[path = "../../prepare.rs"]
mod hardware;

const VECTOR: u8 = 0xf2;
static TRAP: RuntimeTrap = RuntimeTrap::new(VECTOR, RECEPTION.entry());
static RECEPTION: Binding<TimerReception> = Binding::new(
    TimerReception {
        controller: RefCell::new(None),
        captured: AtomicUsize::new(0),
    },
    interrupt,
);

struct TimerReception {
    controller: RefCell<Option<LocalApic<'static>>>,
    captured: AtomicUsize,
}

// The boot processor owns this controller. Ordinary IF stays clear until
// each borrow has ended, and the runtime handler cannot nest.
unsafe impl Sync for TimerReception {}

impl TimerReception {
    fn with_controller<T>(&self, operation: impl FnOnce(&mut LocalApic<'static>) -> T) -> T {
        let mut controller = self.controller.borrow_mut();
        operation(controller.as_mut().expect("admitted timer"))
    }
}

fn interrupt(owner: &TimerReception, mut irq: Irq<'_>) {
    assert_eq!(irq.vector(), VECTOR);
    owner.with_controller(|controller| {
        // Timer programming still belongs to the ordinary masked owner.
        assert_eq!(controller.stop_timer(), Err(Failure::Scope));
        assert_eq!(controller.timer_state(), Some(TimerState::Stopped));
        let mut claim = unsafe { controller.receive_irq(&mut irq) }.unwrap();
        assert_eq!(claim.vector(), VECTOR);
        assert_eq!(
            claim.identity().round(),
            owner.captured.load(Ordering::Relaxed) as u64 + 1
        );
        unsafe { claim.complete() }.unwrap();
        assert_eq!(unsafe { claim.complete() }, Err(Failure::Busy));
    });
    owner.captured.fetch_add(1, Ordering::Release);
}

fn permit_irq() {
    unsafe { asm!("sti", "nop", "cli", options(nostack)) };
}

fn await_vector(expected: usize) {
    let start = q35::tsc();
    let timeout = q35::tsc_frequency();
    while RECEPTION.owner().captured.load(Ordering::Acquire) < expected {
        assert!(q35::tsc().wrapping_sub(start) < timeout);
        permit_irq();
    }
    assert_eq!(RECEPTION.owner().captured.load(Ordering::Acquire), expected);
}

pub(super) fn check(boot: &boot::Boot<'_>) {
    let mut window = q35::local_window(boot);
    unsafe { hardware::prepare(&mut window) };
    let mut local = q35::local_controller(boot, window);
    // The original q35 profile does not expose TSC-deadline. This check
    // must neither touch the deadline MSR nor consume the timer slot.
    assert_eq!(local.deadline_supported(), Ok(false));
    assert_eq!(local.timer_state(), None);

    let vector = local.reserve_vector(VECTOR).unwrap();
    let (vector, reason) = local.reserve_timer(vector).unwrap_err();
    assert_eq!(reason, Failure::Busy);
    // The firmware countdown remains under its original owner and gate
    // until the inherited stop and native drain have succeeded.
    unsafe { local.retire_inherited_timer() }.unwrap();
    local.reserve_timer(vector).unwrap();
    assert_eq!(local.timer_state(), Some(TimerState::Reserved));

    let early = q35::Idtr::read();
    let reception = RECEPTION.owner();
    assert!(reception.controller.replace(Some(local)).is_none());
    unsafe { TRAP.install() }.unwrap();
    let installed = q35::Idtr::read();
    assert_ne!({ installed.base }, { early.base });

    for (index, (ticks, divider)) in [(10_000, Divider::Sixteen), (20_000, Divider::Eight)]
        .into_iter()
        .enumerate()
    {
        let countdown = Countdown {
            ticks: NonZeroU32::new(ticks).unwrap(),
            divider,
        };
        reception.with_controller(|local| {
            unsafe { local.arm_timer(countdown) }.unwrap();
            assert_eq!(local.timer_state(), Some(TimerState::Armed(countdown)));
            assert_eq!(unsafe { local.arm_timer(countdown) }, Err(Failure::Busy));
        });
        // IF stays clear while the real q35 countdown expires. The bounded
        // fixture delay is only an observation window for a relative timer.
        assert!(q35::delay(20_000));
        assert_eq!(RECEPTION.owner().captured.load(Ordering::Acquire), index);
        reception.with_controller(|local| {
            local.stop_timer().unwrap();
            assert_eq!(local.timer_state(), Some(TimerState::Stopped));
            assert_eq!(local.reception(), Reception::Idle);
            assert!(matches!(local.release_timer(), Err(Failure::Busy)));
            assert_eq!(unsafe { local.arm_timer(countdown) }, Err(Failure::Busy));
            assert_eq!(local.timer_state(), Some(TimerState::Stopped));
        });
        await_vector(index + 1);
        reception.with_controller(|local| assert_eq!(local.reception(), Reception::Idle));
        assert_eq!({ q35::Idtr::read().base }, { installed.base });
    }

    let mut local = reception.controller.take().unwrap();
    let returned = local.release_timer().unwrap();
    assert_eq!(returned.vector(), VECTOR);
    local.release_vector(returned).unwrap();
    assert_eq!(local.timer_state(), None);
    for _ in 0..4096 {
        permit_irq();
    }
    assert_eq!(RECEPTION.owner().captured.load(Ordering::Acquire), 2);
    boot_diagnostic::write(
        "PASS local-apic-timer inherited=1 captured=2 eoi=2 drained=2 rearmed=1 released=1\n",
    );
}

pub(super) fn observe_tsc() {
    let basic = __cpuid(0).eax;
    let extended = __cpuid(0x8000_0000).eax;
    let features = __cpuid(1);
    let invariant = extended >= 0x8000_0007 && __cpuid(0x8000_0007).edx & (1 << 8) != 0;
    let crystal = basic >= 0x15 && __cpuid(0x15).ecx != 0;
    assert_ne!(features.edx & (1 << 4), 0);
    assert_eq!(features.ecx & (1 << 24), 0);
    assert!(!invariant && !crystal);
    assert!(matches!(
        unsafe { Tsc::admit() },
        Err(TscFailure::VariableRate)
    ));
    boot_diagnostic::write("PASS q35-tsc counter=1 invariant=0 crystal=0 deadline=0 admitted=0\n");
}

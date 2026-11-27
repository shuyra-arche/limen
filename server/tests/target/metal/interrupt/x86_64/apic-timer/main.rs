#![no_std]
#![no_main]

use core::num::NonZeroU32;
use kernel::{
    entry::boot,
    metal::interrupt::x86_64::internal::apic::{
        Countdown, Divider, Failure, Reception, TimerState,
    },
};
use uefi::prelude::*;
use uefi_fixture::{
    self as fixture,
    interrupt::{self, Window},
    q35,
};

const VECTOR: u8 = 0xf2;

#[entry]
fn main() -> Status {
    fixture::load(|| Ok(()))
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    fixture::check_handoff(&boot, None);
    let mut window = q35::local_window(&boot);
    unsafe { interrupt::prepare(&mut window) };
    let mut local = q35::local_controller(&boot, window);

    let vector = local.reserve_vector(VECTOR).unwrap();
    let (vector, reason) = local.reserve_timer(vector).unwrap_err();
    assert_eq!(reason, Failure::Busy);
    // After ExitBootServices we own the firmware countdown and keep its
    // original gate live through the native stop and IRQ-drain checks.
    unsafe { local.retire_inherited_timer() }.unwrap();
    local.reserve_timer(vector).unwrap();
    let countdown = Countdown {
        ticks: NonZeroU32::new(10_000).unwrap(),
        divider: Divider::Sixteen,
    };
    assert_eq!(local.timer_state(), Some(TimerState::Reserved));
    // The ordinary kernel IDT remains installed while programming. The
    // dedicated test gate is ready before allowing any interrupt entry.
    unsafe { local.arm_timer(countdown) }.unwrap();
    assert_eq!(local.timer_state(), Some(TimerState::Armed(countdown)));
    assert_eq!(unsafe { local.arm_timer(countdown) }, Err(Failure::Busy));
    let entry = unsafe { Window::install(VECTOR) };
    for _ in 0..1_000_000 {
        entry.permit();
        if entry.count() != 0 {
            break;
        }
    }
    assert_eq!(entry.count(), 1);
    drop(entry);

    // A real fixed-vector gate was used. Stop prevents another expiration,
    // but cannot retire the original vector while it is still in service.
    local.stop_timer().unwrap();
    assert_eq!(local.timer_state(), Some(TimerState::Stopped));
    assert!(matches!(local.release_timer(), Err(Failure::Busy)));
    assert_eq!(unsafe { local.receive(VECTOR) }.unwrap().vector(), VECTOR);
    assert_eq!(local.reception(), Reception::Received(VECTOR));
    assert!(matches!(local.release_timer(), Err(Failure::Busy)));
    let mut claim = local.current().unwrap();
    unsafe { claim.complete() }.unwrap();
    assert_eq!(unsafe { claim.complete() }, Err(Failure::Busy));

    let returned = local.release_timer().unwrap();
    assert_eq!(returned.vector(), VECTOR);
    local.release_vector(returned).unwrap();
    assert_eq!(local.timer_state(), None);
    assert_eq!(local.reception(), Reception::Idle);
    let entry = unsafe { Window::install(VECTOR) };
    for _ in 0..10_000 {
        entry.permit();
    }
    assert_eq!(entry.count(), 0);
    drop(entry);

    boot::write(
        "PASS local-apic-timer inherited=1 one-shot=1 received=1 eoi=1 stopped=1 released=1\n",
    );
    boot::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

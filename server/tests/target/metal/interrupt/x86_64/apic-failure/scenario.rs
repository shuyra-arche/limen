use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::metal::interrupt::x86_64::internal::apic;
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::{Minor, StartError, Startup};
use limen_treaty::model::PhysicalAddress;

use super::delay;

static FAILURE_DELAY_CALLS: AtomicUsize = AtomicUsize::new(0);
static OTHER_MINOR: Minor = Minor::new();

fn reject_before_send(_: u32) -> bool {
    false
}

fn reject_after_init(microseconds: u32) -> bool {
    match FAILURE_DELAY_CALLS.fetch_add(1, Ordering::Relaxed) {
        0 => {
            assert_eq!(microseconds, 1);
            delay(microseconds)
        }
        1 => {
            assert_eq!(microseconds, 10_000);
            false
        }
        _ => panic!("a later command followed the uncertain INIT"),
    }
}

fn forbidden(_: usize, _: usize) -> ! {
    panic!("failure fixture must never enter the target")
}

pub(crate) fn check_failure(startup: &mut Startup, target: usize) {
    let Startup::Xapic {
        trampoline, delay, ..
    } = startup;
    let page = PhysicalAddress::new(trampoline.addr().get() as u64);
    *delay = reject_before_send;
    // MP Services identified this inactive target. No SIPI will be sent
    // in either attempt, and the original reserved low page remains live.
    assert_eq!(
        unsafe { crate::MINOR.start(target, forbidden, 0, startup) },
        Err(StartError::Rejected(-1))
    );
    let Startup::Xapic {
        controller, delay, ..
    } = startup;
    assert_eq!(controller.launch_state(), apic::launch::State::Idle);
    assert!(!crate::MINOR.ready());
    *delay = reject_after_init;

    assert_eq!(
        unsafe { crate::MINOR.start(target, forbidden, 0, startup) },
        Err(StartError::Unconfirmed(-1))
    );
    assert_eq!(FAILURE_DELAY_CALLS.load(Ordering::Relaxed), 2);
    let expected = apic::launch::State::Unconfirmed(apic::launch::Effect {
        hart: u8::try_from(target).unwrap(),
        page,
        stage: apic::launch::Stage::Init,
    });
    let Startup::Xapic { controller, .. } = startup;
    assert_eq!(controller.launch_state(), expected);
    assert!(!crate::MINOR.ready());

    // The original launch is retained. A second unused launch record also
    // cannot use the uncertain ICR or rewrite the original trampoline.
    assert_eq!(
        unsafe { crate::MINOR.start(target, forbidden, 1, startup) },
        Err(StartError::Busy)
    );
    assert_eq!(
        unsafe { OTHER_MINOR.start(target, forbidden, 2, startup) },
        Err(StartError::Rejected(-1))
    );
    let Startup::Xapic { controller, .. } = startup;
    assert_eq!(controller.launch_state(), expected);
    assert_eq!(FAILURE_DELAY_CALLS.load(Ordering::Relaxed), 2);
    assert!(!OTHER_MINOR.ready());
    boot_diagnostic::write("PASS local-apic-failure before=1 after-init=1 retained=1 blocked=2\n");
}

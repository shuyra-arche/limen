use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::entry::boot;
use kernel::metal::interrupt::x86_64::internal::apic;
use limen_treaty::model::PhysicalAddress;

use super::delay;

static FAILURE_DELAY_CALLS: AtomicUsize = AtomicUsize::new(0);
static OTHER_MINOR: boot::Minor = boot::Minor::new();

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

pub(crate) fn check_failure(startup: &mut boot::Startup, target: usize) {
    let boot::Startup::Xapic {
        trampoline, delay, ..
    } = startup;
    let page = PhysicalAddress::new(trampoline.addr().get() as u64);
    *delay = reject_before_send;
    // MP Services identified this inactive target. No SIPI will be sent
    // in either attempt, and the original reserved low page remains live.
    assert_eq!(
        unsafe { crate::MINOR.start(target, forbidden, 0, startup) },
        Err(boot::StartError::Rejected(-1))
    );
    let boot::Startup::Xapic {
        controller, delay, ..
    } = startup;
    assert_eq!(controller.state(), apic::State::Idle);
    assert!(!crate::MINOR.ready());
    *delay = reject_after_init;

    assert_eq!(
        unsafe { crate::MINOR.start(target, forbidden, 0, startup) },
        Err(boot::StartError::Unconfirmed(-1))
    );
    assert_eq!(FAILURE_DELAY_CALLS.load(Ordering::Relaxed), 2);
    let expected = apic::State::Unconfirmed(apic::Effect {
        hart: u8::try_from(target).unwrap(),
        page,
        stage: apic::Stage::Init,
    });
    let boot::Startup::Xapic { controller, .. } = startup;
    assert_eq!(controller.state(), expected);
    assert!(!crate::MINOR.ready());

    // The original launch is retained. A second unused launch record also
    // cannot use the uncertain ICR or rewrite the original trampoline.
    assert_eq!(
        unsafe { crate::MINOR.start(target, forbidden, 1, startup) },
        Err(boot::StartError::Busy)
    );
    assert_eq!(
        unsafe { OTHER_MINOR.start(target, forbidden, 2, startup) },
        Err(boot::StartError::Rejected(-1))
    );
    let boot::Startup::Xapic { controller, .. } = startup;
    assert_eq!(controller.state(), expected);
    assert_eq!(FAILURE_DELAY_CALLS.load(Ordering::Relaxed), 2);
    assert!(!OTHER_MINOR.ready());
    boot::write("PASS local-apic-failure before=1 after-init=1 retained=1 blocked=2\n");
}

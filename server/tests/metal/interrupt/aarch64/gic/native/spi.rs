//! One Distributor route closing against a real IAR on a different PE.

use super::{clear_transmit, route_consumer, transmit, wait_for_claim};
use core::arch::asm;
use kernel::metal::{
    interrupt::{
        aarch64::gic::{
            GicV3,
            cpu_interface::CpuInterface,
            distributor::{
                Barrier, Destination, Distributor, Failure as RouteFailure, RetirementReceipt,
                RetirementRequest, Route, Slot, State,
            },
            reception::Failure,
            redistributor::Frame,
        },
        treaty::{ClaimId, Completion, HardwareClaim, RouteControl},
    },
    memory::access::treaty::device::DeviceWindow,
    platform::boot::treaty as boot_diagnostic,
    sync::treaty::LeafLock,
};

static DESTINATION: LeafLock<Option<Destination>> = LeafLock::new(None);
static SOURCE: LeafLock<Option<usize>> = LeafLock::new(None);
static CAPTURED: LeafLock<Option<ClaimId>> = LeafLock::new(None);
static REQUEST: LeafLock<Option<RetirementRequest>> = LeafLock::new(None);
static RECEIPT: LeafLock<Option<RetirementReceipt>> = LeafLock::new(None);

fn poll<T>(mut observe: impl FnMut() -> Option<T>) -> T {
    let start: u64;
    let frequency: u64;
    unsafe {
        asm!("mrs {}, CNTVCT_EL0", "mrs {}, CNTFRQ_EL0", out(reg) start, out(reg) frequency, options(nostack))
    };
    loop {
        if let Some(value) = observe() {
            return value;
        }
        let now: u64;
        unsafe { asm!("mrs {}, CNTVCT_EL0", out(reg) now, options(nostack)) };
        assert!(
            now.wrapping_sub(start) < frequency * 3,
            "remote SPI handoff"
        );
        core::hint::spin_loop();
    }
}

fn send<T>(mailbox: &LeafLock<Option<T>>, message: T) {
    let mut message = Some(message);
    poll(|| {
        mailbox
            .acquire(|slot| {
                assert!(slot.is_none());
                *slot = message.take();
            })
            .ok()
    });
}

fn take<T>(mailbox: &LeafLock<Option<T>>) -> T {
    poll(|| mailbox.acquire(Option::take).ok().flatten())
}

pub(super) fn on_minor(frame: Frame<'static>) -> ! {
    let mut receiver = unsafe { CpuInterface::admit(frame) }
        .unwrap_or_else(|(_, failure)| panic!("minor GIC interface: {failure:?}"));
    send(&DESTINATION, receiver.destination().unwrap());
    let source = take(&SOURCE);
    let captured = wait_for_claim(&mut receiver, source);
    send(&CAPTURED, captured);
    let request = take(&REQUEST);
    let (request, failure) = receiver.observe_retirement(request).unwrap_err();
    assert_eq!(failure, Failure::Busy);

    let mut claim = receiver.claim().expect("original remote SPI");
    assert_eq!(claim.identity(), captured);
    assert_eq!(unsafe { claim.finish() }, Ok(Completion::Settled));
    let receipt = receiver.observe_retirement(request).unwrap();
    assert_eq!(receipt.received_through(), captured.round());
    send(&RECEIPT, receipt);
    loop {
        core::hint::black_box(&mut receiver);
        unsafe { asm!("wfe", options(nostack, nomem)) };
    }
}

pub(super) fn close_remote(
    gic: GicV3<'_>,
    window: DeviceWindow<'_>,
    route: Route<'_>,
    uart: &mut DeviceWindow<'_>,
    major: &CpuInterface<'_>,
    minor: usize,
) {
    let mut slots = [const { Slot::EMPTY }];
    let mut distributor =
        unsafe { Distributor::admit(gic, window, Barrier::native(), &mut slots) }.unwrap();
    let mut binding = unsafe { distributor.reserve(route) }.unwrap();
    let destination = take(&DESTINATION);
    unsafe {
        distributor
            .configure(&mut binding, destination, 0x20)
            .unwrap()
            .publish()
            .unwrap();
    }
    let source = route.intid();
    send(&SOURCE, source);
    transmit(uart);
    let captured = take(&CAPTURED);
    assert_eq!(captured.context(), minor as u64);
    assert_eq!(captured.source(), source as u32);
    assert_eq!(distributor.active(source), Ok(true));

    clear_transmit(uart);
    distributor.control(&mut binding).unwrap().mask().unwrap();
    let request = unsafe { distributor.begin_retirement(&binding) }.unwrap();
    assert_eq!(distributor.state(source), Ok(State::Retiring));
    assert!(matches!(
        unsafe { distributor.begin_retirement(&binding) },
        Err(RouteFailure::Busy)
    ));
    assert_eq!(
        unsafe { distributor.control(&mut binding).unwrap().resume() },
        Err(RouteFailure::State)
    );
    let (request, failure) = major.observe_retirement(request).unwrap_err();
    assert_eq!(failure, Failure::Scope);
    send(&REQUEST, request);
    let receipt = take(&RECEIPT);
    assert_eq!(receipt.received_through(), captured.round());
    assert_eq!(distributor.active(source), Ok(false));
    unsafe { distributor.finish_retirement(&mut binding, receipt) }.unwrap();

    let mut replacement = unsafe { distributor.reserve(route) }.unwrap();
    assert!(distributor.control(&mut binding).is_err());
    let destination = major.destination().unwrap();
    unsafe {
        route_consumer::retire(
            &mut distributor
                .configure(&mut replacement, destination, 0x20)
                .unwrap(),
        )
    };
    boot_diagnostic::write(
        "PASS arm-spi destination=remote masked=1 live-claim-blocked=1 wrong-pe-rejected=1 retired=1 rebound=1\n",
    );
}

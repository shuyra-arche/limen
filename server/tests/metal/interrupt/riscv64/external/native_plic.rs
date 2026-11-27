//! QEMU virt: two physical source routes in the original supervisor context.

use kernel::metal::interrupt::treaty::{
    ClaimConfirmation, ClaimId, Completion, HardwareClaim, Receive,
};
use kernel::metal::platform::boot::treaty as boot_diagnostic;

#[path = "../../claim.rs"]
mod claim_consumer;
#[path = "../../completion.rs"]
mod consumer;
#[path = "../../route.rs"]
mod route_consumer;

use kernel::{
    entry::boot::Boot,
    metal::{
        chrono::riscv64::{description::RiscvTimebase, time::Counter},
        interrupt::riscv64::{
            external::{
                Barrier,
                plic::{
                    ContextSlot, Controller, Destination, Failure, Limits, Route, RouteSlot,
                    RouteState, Source, SourceSlot, State,
                },
            },
            internal::supervisor::Supervisor,
        },
        memory::access::treaty::device::DeviceWindow,
    },
};
use limen_treaty::model::{InternalAddress, PhysicalRange};

const RTC_TIME_LOW: usize = 0x00;
const RTC_TIME_HIGH: usize = 0x04;
const RTC_ALARM_LOW: usize = 0x08;
const RTC_ALARM_HIGH: usize = 0x0c;
const RTC_ENABLE: usize = 0x10;
const RTC_CLEAR_ALARM: usize = 0x14;
const RTC_CLEAR_INTERRUPT: usize = 0x1c;

fn mapped(range: PhysicalRange) -> DeviceWindow<'static> {
    unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())) }.unwrap()
}

fn synchronize() {
    unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) };
}

fn clear_alarm(rtc: &mut DeviceWindow<'_>) {
    unsafe {
        rtc.write_u32(RTC_ENABLE, 0).unwrap();
        rtc.write_u32(RTC_CLEAR_ALARM, 1).unwrap();
        rtc.write_u32(RTC_CLEAR_INTERRUPT, 1).unwrap();
    }
    synchronize();
}

fn arm_alarm(rtc: &mut DeviceWindow<'_>) {
    unsafe {
        let low = rtc.read_u32(RTC_TIME_LOW).unwrap();
        let high = rtc.read_u32(RTC_TIME_HIGH).unwrap();
        let target = ((u64::from(high) << 32) | u64::from(low)).wrapping_add(1_000_000);
        rtc.write_u32(RTC_ALARM_HIGH, (target >> 32) as u32)
            .unwrap();
        rtc.write_u32(RTC_ALARM_LOW, target as u32).unwrap();
        rtc.write_u32(RTC_ENABLE, 1).unwrap();
    }
    synchronize();
}

fn receive(clock: &Counter<'_>) {
    let deadline = clock.read().unwrap() + u64::from(clock.hertz()) * 2;
    assert_eq!(
        unsafe { super::reception::receive(deadline) },
        (1usize << 63) | 9
    );
}

pub(super) fn check(boot: &Boot<'_>, local: &mut Supervisor) {
    let tree = boot.device_tree_description().unwrap();
    let uart = tree.find_path(b"/soc/serial@10000000").unwrap().unwrap();
    assert!(uart.compatible(b"ns16550a").unwrap());
    let rtc = tree
        .nodes()
        .find(|node| node.compatible(b"google,goldfish-rtc").unwrap())
        .unwrap();
    let mut serial = mapped(uart.register(0).unwrap());
    let mut alarm = mapped(rtc.register(0).unwrap());
    clear_alarm(&mut alarm);
    let cpus = boot.riscv_cpus();
    let hart = boot.processor() as u64;
    let uart_source = Source::read(&tree, uart).unwrap();
    let rtc_source = Source::read(&tree, rtc).unwrap();
    let domain = uart_source.controller();
    let destination = Destination::read(&tree, cpus, domain, hart).unwrap();
    let other = Destination::read(&tree, cpus, domain, hart ^ 1).unwrap();
    let route = Route::connect(uart_source, destination).unwrap();
    let rtc_route = Route::connect(rtc_source, destination).unwrap();
    let other_rtc_route = Route::connect(rtc_source, other).unwrap();
    let index = destination.number();
    let other_index = other.number();
    assert_ne!(index, other_index);
    let uart_id = uart_source.number();
    let rtc_id = rtc_source.number();
    assert_ne!(rtc_id, uart_id);
    assert_eq!(rtc_id / 32, uart_id / 32, "sources share an enable word");
    let timebase = RiscvTimebase::describe(cpus).unwrap().unwrap();
    let clock = unsafe { Counter::admit(timebase, hart) }.unwrap();
    let physical = domain.registers().start();
    let mut mapping = mapped(domain.registers());
    // QEMU virt uses the SiFive-compatible three-bit priority/threshold.
    // The chosen policy is nondefault so a constant mechanism cannot pass.
    let limits = Limits::new(7, 7).unwrap();
    let threshold = 0x20_0000 + index * 4096;
    let inherited = unsafe { mapping.read_u32(threshold) }.unwrap();
    let rtc_priority = rtc_id as usize * 4;
    let inherited_rtc_priority = unsafe { mapping.read_u32(rtc_priority) }.unwrap();
    let enable_word = |context| 0x2000 + context * 128 + rtc_id as usize / 32 * 4;
    let rtc_bit = 1 << (rtc_id % 32);
    let mut contexts = [const { ContextSlot::EMPTY }; 2];
    let mut sources = [const { SourceSlot::EMPTY }; 2];
    let mut routes = [const { RouteSlot::EMPTY }; 2];
    let mut controller =
        unsafe { Controller::admit(domain, limits, &mut contexts, &mut sources, &mut routes) }
            .unwrap();
    let mut other_binding = {
        let mut domain = unsafe { controller.access(&mut mapping, Barrier::native()) }.unwrap();
        // The second firmware Hart stays parked and its local reception is
        // masked. Prepare both real contexts without minting its authority.
        let binding = unsafe {
            assert_eq!(domain.prepare(destination, 8), Err(Failure::Scope));
            domain.prepare(destination, 1).unwrap();
            domain.prepare(other, 2).unwrap();
            let mut binding = domain.reserve(other_rtc_route, 4).unwrap();
            assert!(matches!(domain.reserve(rtc_route, 3), Err(Failure::Busy)));
            route_consumer::publish(&mut domain.control(&mut binding).unwrap());
            assert_eq!(
                domain.open(other_index, &mut local.external),
                Err(Failure::Scope)
            );
            binding
        };
        assert_eq!(domain.cancel(other_index), Err(Failure::Busy));
        binding
    };
    assert_eq!(unsafe { mapping.read_u32(rtc_priority) }.unwrap(), 4);
    assert_eq!(
        unsafe { mapping.read_u32(enable_word(index)) }.unwrap() & rtc_bit,
        0
    );
    assert_eq!(
        unsafe { mapping.read_u32(enable_word(other_index)) }.unwrap() & rtc_bit,
        rtc_bit
    );
    let (prior, mut rtc_binding) = {
        let mut domain = unsafe { controller.access(&mut mapping, Barrier::native()) }.unwrap();
        assert_eq!(
            unsafe { domain.open(index, &mut local.timer) },
            Err(Failure::Scope)
        );
        let (mut uart_binding, rtc_binding) = unsafe {
            route_consumer::mask(&mut domain.control(&mut other_binding).unwrap());
            route_consumer::retire(&mut domain.control(&mut other_binding).unwrap());
            domain.cancel(other_index).unwrap();
            assert!(matches!(domain.reserve(route, 8), Err(Failure::Scope)));
            domain.open(index, &mut local.external).unwrap();
            let mut uart_binding = domain.reserve(route, 2).unwrap();
            let mut rtc_binding = domain.reserve(rtc_route, 3).unwrap();
            assert!(domain.control(&mut other_binding).is_err());
            route_consumer::publish(&mut domain.control(&mut uart_binding).unwrap());
            route_consumer::publish(&mut domain.control(&mut rtc_binding).unwrap());
            assert!(matches!(domain.reserve(rtc_route, 3), Err(Failure::Busy)));
            (uart_binding, rtc_binding)
        };
        assert_eq!(unsafe { serial.read_u8(1) }, Ok(0));
        assert_eq!(unsafe { serial.read_u8(3) }.unwrap() & 0x80, 0);
        assert!(domain.claim(index, &local.external).unwrap().is_none());

        let mut prior: Option<ClaimId> = None;
        for round in 0..2 {
            unsafe {
                serial.write_u8(1, 2).unwrap();
                serial.write_u8(0, b' ').unwrap();
            }
            synchronize();
            receive(&clock);
            let receipt = {
                let mut receiver = domain.receiver(destination, &local.external).unwrap();
                let claim = claim_consumer::receive(&mut receiver, uart_id).unwrap();
                assert!(claim.expected());
                claim.identity()
            };
            assert_eq!(receipt.context(), index as u64);
            assert_eq!(receipt.controller(), physical);
            assert!(prior.is_none_or(|old| receipt.round() > old.round()));
            assert_eq!(unsafe { serial.read_u8(2) }.unwrap() & 0x0f, 2);
            unsafe { serial.write_u8(1, 0) }.unwrap();
            assert_eq!(unsafe { serial.read_u8(1) }, Ok(0));
            route_consumer::mask(&mut domain.control(&mut uart_binding).unwrap());
            assert_eq!(domain.route_state(uart_id), Ok(RouteState::Masked));
            assert_eq!(domain.route_state(rtc_id), Ok(RouteState::Enabled));
            let mut receiver = domain.receiver(destination, &local.external).unwrap();
            {
                let mut original = claim_consumer::pending(&mut receiver, receipt);
                assert!(original.expected());
                unsafe { consumer::finish(&mut original, uart_id, Completion::AwaitingAcceptance) };
            }
            let mut original = claim_consumer::pending(&mut receiver, receipt);
            if let Some(old) = prior {
                assert!(
                    HardwareClaim::confirm(&mut original, ClaimConfirmation::observe(old)).is_err()
                );
            }
            // QEMU virt accepts this enabled context's exact completion on the
            // original MMIO write. Device drain is asserted independently.
            unsafe { confirm_accepted(&mut original, receipt) };
            assert!(receiver.pending_claim().unwrap().is_none());
            prior = Some(receipt);
            if round == 0 {
                unsafe { route_consumer::resume(&mut domain.control(&mut uart_binding).unwrap()) };
            }
        }
        unsafe { route_consumer::retire(&mut domain.control(&mut uart_binding).unwrap()) };
        assert_eq!(domain.route_state(rtc_id), Ok(RouteState::Enabled));
        (prior.unwrap(), rtc_binding)
    };
    assert_eq!(unsafe { mapping.read_u32(threshold) }.unwrap(), 1);
    assert_eq!(unsafe { mapping.read_u32(rtc_priority) }.unwrap(), 3);
    assert_eq!(
        unsafe { mapping.read_u32(enable_word(other_index)) }.unwrap() & rtc_bit,
        0
    );
    {
        let mut domain = unsafe { controller.access(&mut mapping, Barrier::native()) }.unwrap();
        route_consumer::mask(&mut domain.control(&mut rtc_binding).unwrap());
    }
    assert_eq!(unsafe { mapping.read_u32(rtc_priority) }.unwrap(), 0);
    assert_eq!(unsafe { mapping.read_u32(threshold) }.unwrap(), 1);
    assert_eq!(
        unsafe { mapping.read_u32(enable_word(index)) }.unwrap() & rtc_bit,
        rtc_bit
    );
    arm_alarm(&mut alarm);
    let pending_word = 0x1000 + rtc_id as usize / 32 * 4;
    let deadline = clock.read().unwrap() + u64::from(clock.hertz()) * 2;
    while unsafe { mapping.read_u32(pending_word) }.unwrap() & rtc_bit == 0 {
        assert!(
            clock.read().unwrap() < deadline,
            "masked RTC did not reach the gateway"
        );
    }
    {
        let mut domain = unsafe { controller.access(&mut mapping, Barrier::native()) }.unwrap();
        assert!(domain.claim(index, &local.external).unwrap().is_none());
        unsafe { route_consumer::resume(&mut domain.control(&mut rtc_binding).unwrap()) };
    }
    receive(&clock);
    let receipt = {
        let mut domain = unsafe { controller.access(&mut mapping, Barrier::native()) }.unwrap();
        let mut receiver = domain.receiver(destination, &local.external).unwrap();
        let claim = claim_consumer::receive(&mut receiver, rtc_id).unwrap();
        assert!(claim.expected());
        assert!(claim.identity().round() > prior.round());
        claim.identity()
    };
    clear_alarm(&mut alarm);
    let mut domain = unsafe { controller.access(&mut mapping, Barrier::native()) }.unwrap();
    let mut receiver = domain.receiver(destination, &local.external).unwrap();
    let mut original = claim_consumer::pending(&mut receiver, receipt);
    unsafe {
        consumer::finish(&mut original, rtc_id, Completion::AwaitingAcceptance);
        confirm_accepted(&mut original, receipt);
    }
    assert!(domain.claim(index, &local.external).unwrap().is_none());
    unsafe {
        route_consumer::mask(&mut domain.control(&mut rtc_binding).unwrap());
        route_consumer::retire(&mut domain.control(&mut rtc_binding).unwrap());
        domain.shut(index, &mut local.external).unwrap();
    }
    assert_eq!(domain.state(index), Ok(State::Shut));
    domain.detach(index).unwrap();
    assert_eq!(unsafe { mapping.read_u32(threshold) }.unwrap(), inherited);
    assert_eq!(
        unsafe { mapping.read_u32(rtc_priority) }.unwrap(),
        inherited_rtc_priority
    );
    boot_diagnostic::write("\nPASS external-route published=2 repeated=0 independent=1\n");
    boot_diagnostic::write(
        "\nPASS plic uart=2 rtc=1 complete=3 contexts=2 shared-enable=1 duplicate=0 restored=1\n",
    );
}

/// # Safety
/// The target has established acceptance by this exact original gateway.
unsafe fn confirm_accepted(claim: &mut impl HardwareClaim, original: ClaimId) {
    assert_eq!(claim.identity(), original);
    assert!(
        HardwareClaim::confirm(claim, ClaimConfirmation::observe(original))
            .is_ok_and(|state| state == Completion::AwaitingAcceptance)
    );
    let evidence = unsafe { ClaimConfirmation::platform_accepted(original) };
    assert!(
        HardwareClaim::confirm(claim, evidence).is_ok_and(|state| state == Completion::Settled)
    );
    assert!(HardwareClaim::confirm(claim, evidence).is_err());
}

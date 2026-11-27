//! QEMU virt: two physical source routes in the original supervisor context.

use kernel::{
    entry::boot::{self, Boot},
    metal::{
        chrono::riscv64::{Counter, RiscvTimebase},
        interrupt::riscv64::{
            external::{
                Barrier,
                plic::{ContextSlot, Controller, Failure, Route, SourceSlot, SourceState, State},
            },
            internal::supervisor::Supervisor,
        },
        memory::mmu::access::device::DeviceWindow,
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

fn mapped(range: PhysicalRange) -> DeviceWindow {
    unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())) }.unwrap()
}

fn synchronize() {
    unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) };
}

fn clear_alarm(rtc: &mut DeviceWindow) {
    unsafe {
        rtc.write_u32(RTC_ENABLE, 0).unwrap();
        rtc.write_u32(RTC_CLEAR_ALARM, 1).unwrap();
        rtc.write_u32(RTC_CLEAR_INTERRUPT, 1).unwrap();
    }
    synchronize();
}

fn arm_alarm(rtc: &mut DeviceWindow) {
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
    let hart = boot.hart() as u64;
    let route = Route::read(&tree, cpus, uart, hart).unwrap();
    let rtc_route = Route::read(&tree, cpus, rtc, hart).unwrap();
    let index = route.context();
    let uart_id = route.source();
    let rtc_id = rtc_route.source();
    assert_eq!(rtc_route.context(), index);
    assert_eq!(rtc_route.domain(), route.domain());
    assert_ne!(rtc_id, uart_id);
    assert_eq!(rtc_id / 32, uart_id / 32, "sources share an enable word");
    let timebase = RiscvTimebase::describe(cpus).unwrap().unwrap();
    let clock = unsafe { Counter::admit(timebase, hart) }.unwrap();
    let mut mapping = mapped(route.registers());
    let mut contexts = [ContextSlot::EMPTY];
    let mut sources = [const { SourceSlot::EMPTY }; 2];
    let mut controller =
        unsafe { Controller::admit(route.controller(), &mut contexts, &mut sources) }.unwrap();
    let prior = {
        let mut domain = unsafe { controller.access(&mut mapping, Barrier::native()) }.unwrap();
        assert_eq!(
            unsafe { domain.attach(route, &local.timer) },
            Err(Failure::Scope)
        );
        unsafe {
            domain.attach(route, &local.external).unwrap();
            domain.reserve(route).unwrap();
            domain.reserve(rtc_route).unwrap();
            assert_eq!(domain.reserve(rtc_route), Err(Failure::Busy));
            domain.enable(uart_id).unwrap();
            domain.enable(rtc_id).unwrap();
            domain.open(index, &mut local.external).unwrap();
        }
        assert_eq!(unsafe { serial.read_u8(1) }, Ok(0));
        assert_eq!(unsafe { serial.read_u8(3) }.unwrap() & 0x80, 0);
        assert!(domain.claim(index, &local.external).unwrap().is_none());

        let mut prior = 0;
        for _ in 0..2 {
            unsafe {
                serial.write_u8(1, 2).unwrap();
                serial.write_u8(0, b' ').unwrap();
            }
            synchronize();
            receive(&clock);
            {
                let claim = domain.claim(index, &local.external).unwrap().unwrap();
                assert!(claim.expected());
                assert_eq!(claim.source(), uart_id);
                assert!(claim.round() > prior);
                prior = claim.round();
            }
            assert!(matches!(
                domain.claim(index, &local.external),
                Err(Failure::Busy)
            ));
            assert_eq!(unsafe { serial.read_u8(2) }.unwrap() & 0x0f, 2);
            unsafe { serial.write_u8(1, 0) }.unwrap();
            assert_eq!(unsafe { serial.read_u8(1) }, Ok(0));
            let mut original = domain.current(index, &local.external).unwrap().unwrap();
            // QEMU virt accepts this enabled context's exact completion on the
            // original MMIO write. Device drain is asserted independently.
            unsafe { original.complete() }.unwrap();
            assert_eq!(unsafe { original.complete() }, Err(Failure::Busy));
            unsafe { original.confirm() }.unwrap();
        }
        unsafe { domain.retire(uart_id) }.unwrap();
        assert_eq!(domain.source(rtc_id), Ok(SourceState::Enabled));
        prior
    };
    arm_alarm(&mut alarm);
    receive(&clock);
    {
        let mut domain = unsafe { controller.access(&mut mapping, Barrier::native()) }.unwrap();
        let claim = domain.claim(index, &local.external).unwrap().unwrap();
        assert!(claim.expected());
        assert_eq!(claim.source(), rtc_id);
        assert!(claim.round() > prior);
    }
    clear_alarm(&mut alarm);
    let mut domain = unsafe { controller.access(&mut mapping, Barrier::native()) }.unwrap();
    let mut original = domain.current(index, &local.external).unwrap().unwrap();
    assert_eq!(
        original.source(),
        rtc_id,
        "original claim survives access return"
    );
    unsafe {
        original.complete().unwrap();
        original.confirm().unwrap();
    }
    assert!(domain.claim(index, &local.external).unwrap().is_none());
    unsafe {
        domain.retire(rtc_id).unwrap();
        domain.shut(index, &mut local.external).unwrap();
    }
    assert_eq!(domain.state(index), Ok(State::Shut));
    domain.detach(index).unwrap();
    boot::write("\nPASS plic uart=2 rtc=1 complete=3 shared-enable=1 duplicate=0 restored=1\n");
}

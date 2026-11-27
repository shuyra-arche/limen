//! QEMU virt: real UART transmit interrupt, original supervisor context.
use kernel::{
    entry::boot::{self, Boot},
    metal::{
        chrono::riscv::{Counter, RiscvTimebase},
        interrupt::external::plic::{Barrier, Context, Failure, Route, State},
        memory::mmu::internal::device::DeviceWindow,
    },
};
use limen_treaty::model::{InternalAddress, PhysicalRange};

fn mapped(range: PhysicalRange) -> DeviceWindow {
    unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())) }.unwrap()
}

pub(super) fn check(
    boot: &Boot<'_>,
    local: &mut kernel::metal::interrupt::internal::supervisor::Supervisor,
) {
    let tree = boot.device_tree_description().unwrap();
    let uart = tree.find_path(b"/soc/serial@10000000").unwrap().unwrap();
    assert!(uart.compatible(b"ns16550a").unwrap());
    let mut serial = mapped(uart.register(0).unwrap());
    let cpus = boot.riscv_cpus();
    let hart = boot.hart() as u64;
    let route = Route::read(&tree, cpus, uart, hart).unwrap();
    let timebase = RiscvTimebase::describe(cpus).unwrap().unwrap();
    let clock = unsafe { Counter::admit(timebase, hart) }.unwrap();
    let mut mapping = mapped(route.registers());
    assert!(matches!(
        unsafe { Context::admit(route, &mut mapping, &mut local.timer, Barrier::native()) },
        Err(Failure::Scope)
    ));
    let mut context =
        unsafe { Context::admit(route, &mut mapping, &mut local.external, Barrier::native()) }
            .unwrap();
    assert_eq!(unsafe { serial.read_u8(1) }, Ok(0));
    assert_eq!(unsafe { serial.read_u8(3) }.unwrap() & 0x80, 0);
    unsafe { context.open() }.unwrap();
    assert!(context.claim().unwrap().is_none());
    let mut prior = 0;
    for _ in 0..2 {
        unsafe {
            serial.write_u8(1, 2).unwrap();
            serial.write_u8(0, b' ').unwrap();
            core::arch::asm!("fence iorw, iorw", options(nostack));
        }
        let deadline = clock.read().unwrap() + u64::from(clock.hertz()) * 2;
        assert_eq!(
            unsafe { super::reception::receive(deadline) },
            (1usize << 63) | 9
        );
        {
            let claim = context.claim().unwrap().unwrap();
            assert_eq!(claim.source(), route.source());
            assert!(claim.round() > prior);
            prior = claim.round();
        }
        assert!(matches!(context.claim(), Err(Failure::Busy)));
        assert_eq!(unsafe { serial.read_u8(2) }.unwrap() & 0x0f, 2);
        unsafe { serial.write_u8(1, 0) }.unwrap();
        assert_eq!(unsafe { serial.read_u8(1) }, Ok(0));
        let mut original = context.current().unwrap();
        // The virt PLIC model accepts this enabled context's completion
        // synchronously on the MMIO write. No physical bridge is assumed.
        unsafe { original.complete() }.unwrap();
        assert_eq!(unsafe { original.complete() }, Err(Failure::Busy));
        unsafe { original.confirm() }.unwrap();
    }
    assert!(context.claim().unwrap().is_none());
    unsafe { context.shut() }.unwrap();
    assert_eq!(context.state(), State::Shut);
    boot::write("\nPASS plic uart=2 complete=2 duplicate=0 restored=1\n");
}

//! QEMU virt hands the guest an identity-mapped physical GIC with a single
//! security state. Keep the PE IRQ/FIQ masks closed and poll its real IAR.
use core::{arch::asm, fmt::Write};
use kernel::{
    entry::boot,
    metal::{
        interrupt::aarch64::gic::{
            GicV3,
            external::{Barrier, Distributor, Route, Slot, State as SpiState},
            internal::reception::{Reception, Redistributor, State},
        },
        memory::mmu::access::device::DeviceWindow,
        platform::description::devicetree::DeviceTree,
    },
};
use limen_treaty::model::{InternalAddress, PhysicalRange};

fn window(range: PhysicalRange) -> DeviceWindow {
    unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())).unwrap() }
}

fn sync() {
    unsafe { asm!("dsb sy", "isb", options(nostack)) }
}

const UART_DATA: usize = 0;
const UART_CONTROL: usize = 0x30;
const UART_MASK: usize = 0x38;
const UART_STATUS: usize = 0x40;
const UART_CLEAR: usize = 0x44;
const UART_ENABLE_TRANSMIT: u32 = 1 | 1 << 8;
const TX_INTERRUPT: u32 = 1 << 5;

fn clear_transmit(uart: &mut DeviceWindow) {
    unsafe {
        uart.write_u32(UART_MASK, 0).unwrap();
        uart.write_u32(UART_CLEAR, TX_INTERRUPT).unwrap();
    }
    sync();
    assert_eq!(unsafe { uart.read_u32(UART_STATUS) }.unwrap(), 0);
}

fn transmit(uart: &mut DeviceWindow) {
    unsafe {
        uart.write_u32(UART_MASK, TX_INTERRUPT).unwrap();
        uart.write_u32(UART_DATA, u32::from(b'\n')).unwrap();
    }
    sync();
}

fn wait_for_claim(receiver: &mut Reception<'_>, expected: usize) {
    let start: u64;
    let frequency: u64;
    unsafe {
        asm!("mrs {}, CNTVCT_EL0", "mrs {}, CNTFRQ_EL0", out(reg) start, out(reg) frequency, options(nostack))
    };
    loop {
        if let Some(claimed) = receiver.receive().unwrap() {
            assert_eq!(claimed.intid(), expected as u32);
            return;
        }
        let now: u64;
        unsafe { asm!("mrs {}, CNTVCT_EL0", out(reg) now, options(nostack)) };
        assert!(
            now.wrapping_sub(start) < frequency * 3,
            "GIC reception timeout"
        );
    }
}

pub(super) fn check(tree: &DeviceTree<'_>, controller: GicV3<'_>, major: usize) {
    let device = tree
        .find_path(b"/pl011@9000000")
        .unwrap()
        .expect("virt console UART");
    let second = tree
        .find_path(b"/pl011@9040000")
        .unwrap()
        .expect("virt secondary UART");
    for uart in [device, second] {
        assert!(uart.compatible(b"arm,pl011").unwrap());
        assert!(uart.enabled().unwrap());
    }
    let route = Route::read(tree, device.id(), controller, 0).unwrap();
    let second_route = Route::read(tree, second.id(), controller, 0).unwrap();
    let id = route.intid();
    let second_id = second_route.intid();
    assert_ne!(second_id, id);
    assert_eq!(second_id / 32, id / 32, "sources share a group word");
    assert_eq!(
        second_id / 16,
        id / 16,
        "sources share a configuration word"
    );
    let mut console = window(device.register(0).unwrap());
    let mut secondary = window(second.register(0).unwrap());
    let secondary_control = unsafe { secondary.read_u32(UART_CONTROL).unwrap() };
    unsafe {
        secondary
            .write_u32(UART_CONTROL, secondary_control | UART_ENABLE_TRANSMIT)
            .unwrap()
    };
    clear_transmit(&mut console);
    clear_transmit(&mut secondary);
    let mut distributor = window(controller.distributor());
    let mut redistributors = window(controller.redistributor());
    // ExitBootServices has relinquished the physical GIC. The CPU masks
    // remain closed and the minor is still off. Restore forwarding and PMR
    // after completing the exact UART interrupt.
    let old_control = unsafe { distributor.read_u32(0).unwrap() };
    assert_eq!(old_control & (1 << 6), 1 << 6);
    let old_mode: u64;
    unsafe { asm!("mrs {}, ICC_CTLR_EL1", out(reg) old_mode, options(nostack)) };
    let old_group: u64;
    let old_priority: u64;
    unsafe {
        asm!("mrs {}, ICC_IGRPEN1_EL1", "mrs {}, ICC_PMR_EL1", out(reg) old_group, out(reg) old_priority, options(nostack));
    }
    sync();
    for split in [false, true] {
        let requested = (old_mode & !2) | (u64::from(split) << 1);
        unsafe { asm!("msr ICC_CTLR_EL1, {}", "isb", in(reg) requested, options(nostack)) };
        let mut redistributor =
            unsafe { Redistributor::locate(controller, tree, &mut redistributors) }.unwrap();
        for _ in 0..4096 {
            if redistributor.wake().is_ok() {
                break;
            }
        }
        redistributor.wake().unwrap();
        unsafe {
            distributor.write_u32(0, old_control | (1 << 1)).unwrap();
            asm!("msr ICC_PMR_EL1, {}", "msr ICC_IGRPEN1_EL1, {}", "isb", in(reg) 0x80u64, in(reg) 1u64, options(nostack));
        }
        sync();
        let mut receiver = unsafe { Reception::admit(redistributor) }.expect("local GIC receiver");
        assert!(receiver.receive().unwrap().is_none());
        let mut slots = [const { Slot::EMPTY }; 2];
        let mut spi = unsafe {
            Distributor::admit(controller, &mut distributor, Barrier::native(), &mut slots)
        }
        .expect("physical Distributor");
        unsafe {
            spi.reserve(route).unwrap();
            spi.reserve(second_route).unwrap();
            assert!(spi.reserve(second_route).is_err());
            spi.configure(id, major as u64, 0x10).unwrap();
            spi.configure(second_id, major as u64, 0x20).unwrap();
            spi.enable(id).unwrap();
            spi.enable(second_id).unwrap();
        }
        for (input, uart) in [(id, &mut console), (second_id, &mut secondary)] {
            assert_eq!(spi.state(input), Ok(SpiState::Enabled));
            transmit(uart);
            wait_for_claim(&mut receiver, input);
            assert_eq!(receiver.state(), State::Claimed(input as u32));
            assert!(receiver.receive().is_err());
            let mut claim = receiver
                .claim()
                .expect("dropped borrow preserves original IAR");
            assert_eq!(claim.intid(), input as u32);
            assert!(unsafe { claim.deactivate() }.is_err());
            assert_eq!(unsafe { uart.read_u32(UART_STATUS) }.unwrap(), TX_INTERRUPT);
            clear_transmit(uart);
            for _ in 0..4096 {
                if spi.disable(input).is_ok() {
                    break;
                }
            }
            assert_eq!(spi.state(input), Ok(SpiState::Masked));
            unsafe { claim.drop_priority().unwrap() };
            assert!(unsafe { claim.drop_priority() }.is_err());
            assert_eq!(spi.active(input), Ok(split));
            if split {
                unsafe { claim.deactivate().unwrap() };
                assert!(unsafe { claim.deactivate() }.is_err());
            }
            assert_eq!(spi.active(input), Ok(false));
            assert_eq!(receiver.state(), State::Empty);
            unsafe { spi.retire(input).unwrap() };
        }
    }
    let bank = id / 32 * 4;
    let bit = (1 << (id % 32)) | (1 << (second_id % 32));
    assert_eq!(
        unsafe { distributor.read_u32(0x300 + bank) }.unwrap() & bit,
        0
    );
    assert_eq!(
        unsafe { distributor.read_u32(0x100 + bank) }.unwrap() & bit,
        0
    );
    unsafe {
        secondary
            .write_u32(UART_CONTROL, secondary_control)
            .unwrap();
        asm!("msr ICC_CTLR_EL1, {}", "isb", in(reg) old_mode, options(nostack));
        distributor.write_u32(0, old_control).unwrap();
        asm!("msr ICC_PMR_EL1, {}", "msr ICC_IGRPEN1_EL1, {}", "isb", in(reg) old_priority, in(reg) old_group, options(nostack));
    }
    sync();
    boot::console()
        .unwrap()
        .write_str("PASS virt interrupt uart-primary=2 uart-secondary=2 shared-words=1 iar=4 eoi=4 dir=2 masked=1\n")
        .unwrap();
}

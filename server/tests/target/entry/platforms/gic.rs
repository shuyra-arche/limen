//! QEMU virt hands the guest an identity-mapped physical GIC with a single
//! security state. Keep the PE IRQ/FIQ masks closed and poll its real IAR.
use core::{arch::asm, fmt::Write};
use kernel::{
    entry::boot,
    metal::{
        interrupt::{
            external::gic::{Barrier, Route, Spi, State as SpiState},
            internal::gic::{
                GicV3,
                reception::{Reception, Redistributor, State},
            },
        },
        memory::mmu::internal::device::DeviceWindow,
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

pub(super) fn check(tree: &DeviceTree<'_>, controller: GicV3<'_>, major: usize) {
    let device = tree
        .nodes()
        .find(|node| node.compatible(b"arm,pl011").unwrap() && node.enabled().unwrap())
        .expect("virt UART");
    let route = Route::read(tree, device.id(), controller, 0).unwrap();
    let id = route.intid();
    let mut uart = window(device.register(0).unwrap());
    // This polling console is quiescent. Mask TX and clear its old latched cause.
    unsafe {
        uart.write_u32(0x38, 0).unwrap();
        uart.write_u32(0x44, 1 << 5).unwrap();
    }
    sync();
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
        let mut spi = unsafe { Spi::admit(route, &mut distributor, Barrier::native()) }
            .expect("idle UART SPI");
        unsafe {
            spi.configure(major as u64, 0x10).unwrap();
            spi.enable().unwrap();
        }
        // A real transmit through the same PL011 used for diagnostics
        // raises its TX cause as the transmit FIFO drains.
        unsafe {
            uart.write_u32(0x38, 1 << 5).unwrap();
            uart.write_u32(0, u32::from(b'\n')).unwrap();
        }
        sync();
        let start: u64;
        let frequency: u64;
        unsafe {
            asm!("mrs {}, CNTVCT_EL0", "mrs {}, CNTFRQ_EL0", out(reg) start, out(reg) frequency, options(nostack))
        };
        loop {
            if let Some(claimed) = receiver.receive().unwrap() {
                assert_eq!(claimed.intid(), id as u32, "original UART INTID");
                break;
            }
            let now: u64;
            unsafe { asm!("mrs {}, CNTVCT_EL0", out(reg) now, options(nostack)) };
            assert!(
                now.wrapping_sub(start) < frequency * 3,
                "UART reception timeout"
            );
        }
        assert_eq!(receiver.state(), State::Claimed(id as u32));
        assert!(receiver.receive().is_err());
        let mut claim = receiver
            .claim()
            .expect("dropped borrow preserves original IAR");
        assert_eq!(claim.intid(), id as u32);
        assert!(unsafe { claim.deactivate() }.is_err());
        assert_eq!(unsafe { uart.read_u32(0x40).unwrap() }, 1 << 5);
        unsafe {
            uart.write_u32(0x38, 0).unwrap();
            uart.write_u32(0x44, 1 << 5).unwrap();
        }
        sync();
        assert_eq!(unsafe { uart.read_u32(0x40).unwrap() }, 0);
        for _ in 0..4096 {
            if spi.disable().is_ok() {
                break;
            }
        }
        assert_eq!(spi.state(), SpiState::Masked);
        unsafe { claim.drop_priority().unwrap() };
        assert!(unsafe { claim.drop_priority() }.is_err());
        assert_eq!(spi.active(), split);
        if split {
            unsafe { claim.deactivate().unwrap() };
            assert!(unsafe { claim.deactivate() }.is_err());
        }
        assert!(!spi.active());
        assert_eq!(receiver.state(), State::Empty);
    }
    let bank = id / 32 * 4;
    let bit = 1 << (id % 32);
    assert_eq!(
        unsafe { distributor.read_u32(0x300 + bank) }.unwrap() & bit,
        0
    );
    assert_eq!(
        unsafe { distributor.read_u32(0x100 + bank) }.unwrap() & bit,
        0
    );
    unsafe {
        asm!("msr ICC_CTLR_EL1, {}", "isb", in(reg) old_mode, options(nostack));
        distributor.write_u32(0, old_control).unwrap();
        asm!("msr ICC_PMR_EL1, {}", "msr ICC_IGRPEN1_EL1, {}", "isb", in(reg) old_priority, in(reg) old_group, options(nostack));
    }
    sync();
    boot::console()
        .unwrap()
        .write_str("PASS virt interrupt uart=1 spi=1 iar=1 eoi=1 dir=1 masked=1\n")
        .unwrap();
}

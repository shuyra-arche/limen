//! The q35 guest owns COM1 and receives its actual THRE interrupt on the BSP.

use kernel::metal::interrupt::treaty::{ClaimConfirmation, Completion, HardwareClaim};

#[path = "../../../../completion.rs"]
mod consumer;
#[path = "../../../../route.rs"]
mod route_consumer;

use core::arch::asm;
use kernel::metal::{
    interrupt::x86_64::{
        external::ioapic::{Controller, route::State},
        internal::apic::{Failure, LocalApic, Reception},
    },
    memory::access::treaty::device::DeviceWindow,
    platform::description::treaty::acpi::Madt,
};
use limen_treaty::model::{ByteLength, InternalAddress};
use uefi_fixture::q35::io;

use super::window::Window;

const VECTOR: u8 = 0xf1;
const UART: u16 = 0x3f8;
unsafe fn input(port: u16) -> u8 {
    let value: u8;
    unsafe { asm!("in al, dx", in("dx") port, out("al") value, options(nostack, preserves_flags)) };
    value
}

pub(super) fn check(madt: Madt<'_>, processor: usize, local: &mut LocalApic<'_>) {
    let io = madt.io_apics().next().unwrap();
    let range = io.address.range(ByteLength::new(4096)).unwrap();
    let mut window =
        unsafe { DeviceWindow::admit(range, InternalAddress::new(io.address.get())) }.unwrap();
    let mut controller = unsafe { Controller::admit(madt, &mut window) }.unwrap();
    let route = controller
        .isa_route(4, u8::try_from(processor).unwrap(), VECTOR)
        .unwrap();
    let lease = local.reserve_vector(VECTOR).unwrap();
    assert!(matches!(local.reserve_vector(VECTOR), Err(Failure::Busy)));
    let mut binding = controller.reserve(route, lease).unwrap();
    assert_eq!(controller.state(route.gsi), Ok(State::Reserved(route)));
    unsafe {
        io::write8(UART + 3, 3); // 8N1, DLAB clear
        io::write8(UART + 1, 0);
        io::write8(UART + 4, 8); // OUT2 gates the ISA interrupt
        for second_round in [false, true] {
            let window = Window::install(VECTOR);
            if second_round {
                route_consumer::resume(&mut controller.control(&mut binding).unwrap());
            } else {
                route_consumer::publish(&mut controller.configure(&mut binding).unwrap());
            }
            // A fresh THRE condition, with the transmitter idle.
            assert_ne!(input(UART + 5) & 0x20, 0);
            io::write8(UART + 1, 2);
            // Allow only a bounded interval for the actual interrupt entry. No
            // software 'int' instruction can set the xAPIC ISR bit checked below.
            for _ in 0..1_000_000 {
                window.permit();
                if window.count() != 0 {
                    break;
                }
            }
            assert_eq!(window.count(), 1);
            drop(window);
            assert!(matches!(local.receive(0xff), Err(Failure::Scope)));
            assert!(matches!(local.receive(VECTOR - 1), Err(Failure::Scope)));
            assert_eq!(local.reception(), Reception::Idle);
            let receipt = local.receive(VECTOR).unwrap().identity();
            assert_eq!(receipt.source(), u32::from(VECTOR));
            assert_eq!(local.reception(), Reception::Received(VECTOR));
            assert!(matches!(local.receive(VECTOR), Err(Failure::Busy)));
            route_consumer::mask(&mut controller.control(&mut binding).unwrap());
            assert_eq!(input(UART + 2) & 0xf, 2);
            io::write8(UART + 1, 0);
            let mut claim = local.current().unwrap();
            assert_eq!(claim.identity(), receipt);
            consumer::finish(&mut claim, u32::from(VECTOR), Completion::Settled);
            assert!(
                HardwareClaim::confirm(&mut claim, ClaimConfirmation::observe(receipt)).is_err()
            );
            assert!(local.current().is_none());
        }
        let lease = route_consumer::retire(&mut controller.control(&mut binding).unwrap());
        local.release_vector(lease).unwrap();
    }
    let fresh = local.reserve_vector(VECTOR).unwrap();
    let mut replacement = controller.reserve(route, fresh).unwrap();
    assert!(controller.control(&mut binding).is_err());
    let fresh = controller.cancel(&mut replacement).unwrap();
    assert!(controller.cancel(&mut replacement).is_err());
    local.release_vector(fresh).unwrap();
    assert_eq!(local.reception(), Reception::Idle);
    assert_eq!(controller.state(route.gsi), Ok(State::Vacant));
    kernel::metal::platform::boot::treaty::write(
        "PASS q35 interrupt uart=2 ioapic=1 vector=1 eoi=2 masked=1 resumed=1\n",
    );
}

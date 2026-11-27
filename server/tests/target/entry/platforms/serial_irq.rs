//! The q35 guest owns COM1 and receives its actual THRE interrupt on the BSP.

use super::{Idtr, io};
use core::{
    arch::{asm, naked_asm},
    sync::atomic::{AtomicU32, Ordering},
};
use kernel::metal::{
    interrupt::{
        external::ioapic::{Controller, State},
        internal::apic::{Failure, LocalApic, Reception},
    },
    memory::mmu::internal::device::DeviceWindow,
    platform::description::acpi::Madt,
};
use limen_treaty::model::{ByteLength, InternalAddress};

const VECTOR: u8 = 0xf1;
const UART: u16 = 0x3f8;
static RECEIVED: AtomicU32 = AtomicU32::new(0);
static mut GATES: [[u8; 16]; 256] = [[0; 16]; 256];

unsafe fn input(port: u16) -> u8 {
    let value: u8;
    unsafe { asm!("in al, dx", in("dx") port, out("al") value, options(nostack, preserves_flags)) };
    value
}

/// The freshly booted single BSP owns both PICs and this UC xAPIC mapping.
/// Only class 15 is admitted in the short external interrupt test window.
pub(super) unsafe fn prepare_local(window: &mut DeviceWindow) {
    unsafe {
        io::write8(0x21, 0xff);
        io::write8(0xa1, 0xff);
        window.write_u32(0x80, 0xe0).unwrap();
        window.write_u32(0xf0, 0x1ff).unwrap();
    }
}

#[unsafe(naked)]
unsafe extern "C" fn receive() -> ! {
    // The fixed non-nested entry owns a single preallocated observation.
    // The gate's IF remains clear; clear saved IF for the return as well.
    naked_asm!(
        "inc dword ptr [rip + {count}]",
        "and qword ptr [rsp + 16], -513",
        "iretq",
        count = sym RECEIVED,
    );
}

fn gate(original: &Idtr) -> Idtr {
    assert_eq!({ original.limit }, 4095);
    let gates = (&raw mut GATES).cast::<[u8; 16]>();
    unsafe { gates.copy_from_nonoverlapping(original.base as *const [u8; 16], 256) };
    let gate = unsafe { &mut *gates.add(usize::from(VECTOR)) };
    let address = receive as *const () as u64;
    gate[..2].copy_from_slice(&(address as u16).to_le_bytes());
    gate[6..8].copy_from_slice(&((address >> 16) as u16).to_le_bytes());
    gate[8..12].copy_from_slice(&((address >> 32) as u32).to_le_bytes());
    Idtr {
        limit: original.limit,
        base: gates.addr(),
    }
}

pub(super) fn check(madt: Madt<'_>, hart: usize, local: &mut LocalApic) {
    let io = madt.io_apics().next().unwrap();
    let range = io.address.range(ByteLength::new(4096)).unwrap();
    let mut window =
        unsafe { DeviceWindow::admit(range, InternalAddress::new(io.address.get())) }.unwrap();
    let mut controller = unsafe { Controller::admit(madt, &mut window) }.unwrap();
    let route = controller
        .isa_route(4, u8::try_from(hart).unwrap(), VECTOR)
        .unwrap();
    let original = Idtr::read();
    let temporary = gate(&original);
    unsafe {
        io::write8(UART + 3, 3); // 8N1, DLAB clear
        io::write8(UART + 1, 0);
        io::write8(UART + 4, 8); // OUT2 gates the ISA interrupt
        controller.configure(route).unwrap();
        asm!("lidt [{}]", in(reg) &temporary, options(nostack, preserves_flags));
        controller.enable(route.gsi).unwrap();
        // A fresh THRE condition, with the transmitter idle.
        assert_ne!(input(UART + 5) & 0x20, 0);
        io::write8(UART + 1, 2);
        // Allow only a bounded interval for the actual interrupt entry. No
        // software 'int' instruction can set the xAPIC ISR bit checked below.
        for _ in 0..1_000_000 {
            asm!("sti", "nop", "cli", options(nostack));
            if RECEIVED.load(Ordering::Acquire) != 0 {
                break;
            }
        }
        asm!("lidt [{}]", in(reg) &original, options(nostack, preserves_flags));
        assert_eq!(RECEIVED.load(Ordering::Acquire), 1);
        assert!(matches!(local.receive(0xff), Err(Failure::Scope)));
        assert!(matches!(local.receive(VECTOR - 1), Err(Failure::Scope)));
        assert_eq!(local.reception(), Reception::Idle);
        assert_eq!(local.receive(VECTOR).unwrap().vector(), VECTOR);
        assert_eq!(local.reception(), Reception::Received(VECTOR));
        assert!(matches!(local.receive(VECTOR), Err(Failure::Busy)));
        controller.mask(route.gsi).unwrap();
        assert_eq!(input(UART + 2) & 0xf, 2);
        io::write8(UART + 1, 0);
        let mut claim = local.current().unwrap();
        claim.complete().unwrap();
        assert_eq!(claim.complete(), Err(Failure::Busy));
        controller.retire(route.gsi).unwrap();
    }
    assert_eq!(local.reception(), Reception::Idle);
    assert_eq!(controller.state(route.gsi), Ok(State::Vacant));
    kernel::entry::boot::write("PASS q35 interrupt uart=1 ioapic=1 vector=1 eoi=1 masked=1\n");
}

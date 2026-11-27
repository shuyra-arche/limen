//! One stopped EDU function, one IVHD and a resident single-level table.

#[path = "../../../../../support/paging/retirement.rs"]
mod retirement;

use super::edu::{self, Endpoint, REQUESTER};
use core::arch::x86_64::_mm_mfence;
use kernel::entry::boot;
use kernel::metal::{
    memory::mmu::{
        Unbind,
        access::device::DeviceWindow,
        x86_64::external::amdvi::{
            Admissions, Binding, Completion, Context, Controller, Error, Queue, Storage, Table,
        },
    },
    platform::description::acpi::Ivhd,
};
use limen_treaty::model::{InternalAddress, PhysicalAddress};

#[repr(C, align(4096))]
struct Page([u64; 512]);
static mut DEVICES: Table = Table::DENIED;
static mut COMMANDS: Queue = Queue::EMPTY;
static mut COMPLETION: Completion = Completion::zeroed();
static mut BINDINGS: [Option<Binding>; 2] = [None; 2];
static mut PAGES: Page = Page([0; 512]);
static mut PAYLOAD: Page = Page([0; 512]);

fn pin<T>(p: *mut T) -> PhysicalAddress {
    PhysicalAddress::new(p.addr() as u64)
}

fn settle(controller: &mut Controller<'_>, mut result: Result<(), Error>) {
    for _ in 0..100_000 {
        match result {
            Ok(()) => return,
            Err(Error::Pending) => result = controller.poll(),
            Err(error) => panic!("AMD-Vi command: {error:?}"),
        }
    }
    panic!("AMD-Vi command timed out");
}

pub(super) fn check(unit: Ivhd<'_>) {
    assert!(unit.segment() == 0 && unit.selects(REQUESTER));
    let capability = edu::config(unit.device(), unit.capability());
    assert_eq!(capability & 0xff, 0x0f);
    let low = edu::config(unit.device(), unit.capability() + 4);
    let high = edu::config(unit.device(), unit.capability() + 8);
    let base = u64::from(low & !0x3fff) | (u64::from(high) << 32);
    assert_eq!(base, unit.registers().start().get());
    let mut window =
        unsafe { DeviceWindow::admit(unit.registers(), InternalAddress::new(base)) }.unwrap();
    let mut endpoint = Endpoint::open();
    let data = (&raw mut PAYLOAD).cast::<u32>();
    let address = u32::try_from(data.addr()).unwrap();
    const PATTERN: u32 = 0xa316_5b92;
    const IOVA: u32 = 0x10_0000;
    assert_ne!(address, IOVA);
    unsafe {
        data.write_volatile(PATTERN);
        _mm_mfence();
        endpoint.read_from_ram(address);
        data.write_volatile(!PATTERN);
        _mm_mfence();
        endpoint.write_to_ram(address);
        assert_eq!(data.read_volatile(), PATTERN);
    }
    let (devices, commands, completion) =
        (&raw mut DEVICES, &raw mut COMMANDS, &raw mut COMPLETION);
    let storage = unsafe {
        Storage {
            table: (&mut *devices, pin(devices)),
            queue: (&mut *commands, pin(commands)),
            completion: (&mut *completion, pin(completion)),
        }
    };
    // The fixture owns just this physical unit through guest shutdown.
    let mut admissions = Admissions::new(unit);
    let mut controller = unsafe {
        Controller::admit(
            unit,
            &mut window,
            storage,
            &mut *(&raw mut BINDINGS),
            &mut admissions,
        )
    }
    .unwrap();
    let result = controller.install_blocked();
    settle(&mut controller, result);
    unsafe {
        data.write_volatile(!PATTERN);
        _mm_mfence();
        endpoint.write_to_ram(address);
        assert_eq!(data.read_volatile(), !PATTERN);
    }
    let root = &raw mut PAGES;
    unsafe {
        root.cast::<u64>()
            .add((IOVA >> 12) as usize)
            .write(pin(data).get() | (3 << 61) | 1);
    }
    let context = Context {
        requester: REQUESTER,
        root: pin(root),
        domain: 1,
        levels: 1,
    };
    let result = unsafe { controller.bind(context) };
    settle(&mut controller, result);
    let binding = controller.binding(REQUESTER).unwrap();
    assert_eq!(binding.context(), context);
    unsafe {
        endpoint.write_to_ram(IOVA);
        assert_eq!(data.read_volatile(), PATTERN);
    }
    let ticket = unsafe {
        retirement::retire(&mut controller, binding, |controller| {
            assert_eq!(controller.binding(REQUESTER), None);
        })
    };
    assert_eq!(controller.binding(REQUESTER), None);
    unsafe {
        data.write_volatile(!PATTERN);
        _mm_mfence();
        endpoint.write_to_ram(IOVA);
        assert_eq!(data.read_volatile(), !PATTERN);
    }
    let result = unsafe { controller.bind(context) };
    settle(&mut controller, result);
    let rebound = controller.binding(REQUESTER).unwrap();
    assert_ne!(binding, rebound);
    assert!(matches!(
        unsafe { controller.retire(binding) },
        Err(Error::Stale)
    ));
    assert_eq!(controller.binding(REQUESTER), Some(rebound));
    unsafe {
        endpoint.write_to_ram(IOVA);
        assert_eq!(data.read_volatile(), PATTERN);
    }
    unsafe {
        retirement::retire(&mut controller, rebound, |controller| {
            assert_eq!(controller.observe_retirement(&ticket), Err(Error::Stale));
        });
    }
    boot::write(
        "PASS amdvi-dma requester=00:06.0 denied=1 translated=1 unbound=1 stale-rejected=1\n",
    );
}

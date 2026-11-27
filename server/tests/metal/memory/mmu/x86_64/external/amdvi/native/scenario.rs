//! One stopped EDU function, one IVHD and a resident single-level table.

use kernel::metal::platform::boot::treaty as boot_diagnostic;
#[path = "../../../../external/binding.rs"]
mod binding;

use super::edu::{self, Endpoint, REQUESTER};
use core::arch::x86_64::_mm_mfence;
use kernel::metal::{
    memory::access::treaty::device::DeviceWindow,
    memory::mmu::{
        treaty::{BindingControl, BindingProgress, BindingRejection},
        x86_64::external::amdvi::{
            Controller, Error, Storage,
            binding::{Admissions, Binding, Context},
            directory::Table,
            queue::{Completion, Queue},
        },
    },
    platform::description::treaty::acpi::Ivhd,
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
    let binding = unsafe { binding::bind(&mut controller, context) };
    assert_eq!(controller.binding(REQUESTER), Some(binding));
    assert_eq!(binding.context(), context);
    unsafe {
        endpoint.write_to_ram(IOVA);
        assert_eq!(data.read_volatile(), PATTERN);
    }
    let ticket = unsafe {
        binding::retire(&mut controller, binding, |controller| {
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
    let rebound = unsafe { binding::bind(&mut controller, context) };
    assert_ne!(binding, rebound);
    assert!(matches!(
        unsafe { controller.begin_retirement(binding) },
        Err(BindingRejection::Rejected(Error::Stale))
    ));
    assert_eq!(controller.binding(REQUESTER), Some(rebound));
    unsafe {
        endpoint.write_to_ram(IOVA);
        assert_eq!(data.read_volatile(), PATTERN);
    }
    unsafe {
        binding::retire(&mut controller, rebound, |controller| {
            assert_eq!(controller.observe_change(ticket), BindingProgress::Stale);
        });
    }
    boot_diagnostic::write(
        "PASS amdvi-dma requester=00:06.0 denied=1 translated=1 unbound=1 stale-rejected=1\n",
    );
}

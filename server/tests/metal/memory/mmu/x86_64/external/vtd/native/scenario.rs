//! Two q35 endpoints share one VT-d controller and retire exact bindings.

use kernel::metal::platform::boot::treaty as boot_diagnostic;
#[path = "../../../../../../../target/support/paging/binding.rs"]
mod binding;

use super::edu::{Endpoint, REQUESTER};
use core::arch::x86_64::{__cpuid, _mm_clflush, _mm_mfence};
use kernel::metal::{
    memory::access::treaty::device::DeviceWindow,
    memory::mmu::{
        treaty::{BindingControl, BindingEffect, BindingProgress, BindingRejection},
        x86_64::external::vtd::{
            Controller, Error,
            binding::{Context, Refresh, Sequence},
            command::{Change, CommandId},
            directory::{Storage, Table},
            translation::{Format, Visibility},
        },
    },
    platform::description::treaty::acpi::RemappingUnit,
};
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress};

const SECOND: u16 = 7 << 3;
const PATTERN: u32 = 0x95a6_3cd1;
const OTHER_PATTERN: u32 = 0x2489_c03b;
const SENTINEL: u32 = !PATTERN;

// Within the EDU DMA mask, outside the 128 MiB guest RAM and all device BARs.
const IOVA: u32 = 0x0de0_0000;

#[repr(C, align(4096))]
struct Page([u64; 512]);

#[repr(C, align(4096))]
struct DataPage([u32; 1024]);

#[repr(C)]
struct Tables {
    top: Page,
    middle: Page,
    leaf: Page,
}

impl Tables {
    const EMPTY: Self = Self {
        top: Page([0; 512]),
        middle: Page([0; 512]),
        leaf: Page([0; 512]),
    };
}

// The major owns these dedicated pages. All survive hardware use through shutdown.
static mut ROOT: Table = Table::EMPTY;
static mut CONTEXTS: Table = Table::EMPTY;
static mut PRIMARY_TABLES: Tables = Tables::EMPTY;
static mut SECOND_TABLES: Tables = Tables::EMPTY;
static mut PRIMARY_PAYLOAD: DataPage = DataPage([0; 1024]);
static mut SECOND_PAYLOAD: DataPage = DataPage([0; 1024]);

fn physical<T>(page: *mut T) -> PhysicalAddress {
    PhysicalAddress::new(page.addr() as u64)
}

fn translation(
    format: Format,
    tables: *mut Tables,
    data: *mut DataPage,
) -> (*mut u64, PhysicalAddress) {
    let (l3, l2, l1) = unsafe {
        (
            &raw mut (*tables).top,
            &raw mut (*tables).middle,
            &raw mut (*tables).leaf,
        )
    };
    let leaf = unsafe { l1.cast::<u64>().add(((IOVA >> 12) & 511) as usize) };
    unsafe {
        l3.cast::<u64>()
            .add(((IOVA >> 30) & 511) as usize)
            .write(format.table_entry(physical(l2)).unwrap());
        l2.cast::<u64>()
            .add(((IOVA >> 21) & 511) as usize)
            .write(format.table_entry(physical(l1)).unwrap());
        leaf.write(format.writable_leaf(physical(data)).unwrap());
    }
    let features = __cpuid(1);
    assert_ne!(features.edx & (1 << 19), 0);
    let line = ((features.ebx >> 8) & 0xff) as usize * 8;
    assert!(line.is_power_of_two());
    for page in [l3, l2, l1] {
        for offset in (0..4096).step_by(line) {
            unsafe { _mm_clflush(page.cast::<u8>().add(offset)) };
        }
    }
    unsafe { _mm_mfence() };
    (leaf, physical(l3))
}

fn store(word: *mut u32, value: u32) {
    unsafe {
        word.write_volatile(value);
        _mm_mfence();
    }
}

fn before_translation(endpoint: &mut Endpoint, word: *mut u32, pattern: u32) {
    let address = u32::try_from(word.addr()).unwrap();
    assert_ne!(address, IOVA);
    store(word, pattern);
    unsafe { endpoint.read_from_ram(address) };
    store(word, SENTINEL);
    unsafe { endpoint.write_to_ram(address) };
    assert_eq!(unsafe { word.read_volatile() }, pattern);
}

fn await_change(controller: &mut Controller<'_>, command: CommandId) -> Change {
    for _ in 0..100_000 {
        match controller.observe(command) {
            Err(Error::Pending) => core::hint::spin_loop(),
            result => return result.unwrap(),
        }
    }
    panic!("VT-d invalidation timed out");
}

fn dma_result(endpoint: &mut Endpoint, destination: *mut u32, expected: u32) {
    store(destination, SENTINEL);
    unsafe { endpoint.write_to_ram(IOVA) };
    assert_eq!(unsafe { destination.read_volatile() }, expected);
}

fn prove_fault(controller: &mut Controller<'_>, requester: u16, reason: u8) {
    let fault = controller
        .primary_fault()
        .unwrap()
        .expect("recorded VT-d fault");
    assert_eq!(fault.requester(), requester);
    assert_eq!(fault.reason(), reason);
    assert_eq!(fault.address(), u64::from(IOVA & !0xfff));
    assert!(fault.write());
    unsafe { controller.acknowledge_fault(fault) }.unwrap();
    assert_eq!(controller.primary_fault().unwrap(), None);
}

pub(super) fn check(unit: RemappingUnit<'_>, registers: &mut DeviceWindow<'_>) {
    let mut primary = Endpoint::open();
    let mut secondary = Endpoint::open_at(SECOND);
    let first_word = (&raw mut PRIMARY_PAYLOAD).cast::<u32>();
    let second_word = (&raw mut SECOND_PAYLOAD).cast::<u32>();
    before_translation(&mut primary, first_word, PATTERN);
    before_translation(&mut secondary, second_word, OTHER_PATTERN);

    let root = &raw mut ROOT;
    let contexts = &raw mut CONTEXTS;
    let mut slots = [None, None];
    let mut sequence = Sequence::FRESH;
    let storage = Storage {
        bus: 0,
        root: (unsafe { &mut *root }, physical(root)),
        contexts: (unsafe { &mut *contexts }, physical(contexts)),
        bindings: &mut slots,
        sequence: &mut sequence,
    };
    let visibility = Visibility::native().expect("q35 CLFLUSH discovery");
    let mut controller =
        unsafe { Controller::admit(unit, registers, storage, visibility) }.unwrap();
    let format = controller
        .format39()
        .expect("q35 39-bit second-level translation");
    let (leaf, first_root) = translation(format, &raw mut PRIMARY_TABLES, &raw mut PRIMARY_PAYLOAD);
    let (_, second_root) = translation(format, &raw mut SECOND_TABLES, &raw mut SECOND_PAYLOAD);
    unsafe { controller.install_legacy() }.unwrap();
    assert_eq!(controller.primary_fault().unwrap(), None);

    let first_context = Context {
        requester: REQUESTER,
        domain: 1,
        root: first_root,
    };
    let second_context = Context {
        requester: SECOND,
        domain: 2,
        root: second_root,
    };
    let first_command = unsafe { controller.begin_bind(first_context) }.unwrap();
    let Some((original, Change::Bound(first))) = controller.pending() else {
        panic!("first binding not retained")
    };
    assert_eq!(first_command, original);
    assert_eq!(controller.binding(REQUESTER), None);
    assert_eq!(
        await_change(&mut controller, first_command),
        Change::Bound(first)
    );
    assert_eq!(controller.binding(REQUESTER), Some(first));
    assert_eq!(
        unsafe {
            controller.begin_bind(Context {
                domain: first_context.domain,
                ..second_context
            })
        },
        Err(Error::Occupied)
    );
    let command = unsafe { controller.begin_bind(second_context) }.unwrap();
    assert_eq!(controller.observe(first_command), Err(Error::Stale));
    assert_eq!(controller.binding(REQUESTER), Some(first));
    let Change::Bound(second) = await_change(&mut controller, command) else {
        panic!("second binding not completed")
    };
    assert_eq!(controller.binding(SECOND), Some(second));

    dma_result(&mut primary, first_word, PATTERN);
    dma_result(&mut secondary, second_word, OTHER_PATTERN);
    unsafe {
        leaf.write_volatile(
            format
                .read_only_leaf(physical(&raw mut PRIMARY_PAYLOAD))
                .unwrap(),
        );
        _mm_clflush(leaf.cast());
        _mm_mfence();
    }
    let scope = Refresh::GlobalTranslations;
    let command = unsafe { controller.begin_refresh(first, scope) }.unwrap();
    assert_eq!(controller.observe(first_command), Err(Error::Stale));
    assert_eq!(
        binding::settle(&mut controller, command),
        BindingEffect::Refreshed(first, scope)
    );
    dma_result(&mut primary, first_word, SENTINEL);
    prove_fault(&mut controller, REQUESTER, 5);
    dma_result(&mut secondary, second_word, OTHER_PATTERN);

    let ticket = unsafe {
        binding::retire(&mut controller, second, |controller| {
            let (_, change) = controller.pending().expect("original VT-d retirement");
            assert_eq!(change, Change::Unbound(second));
            assert_eq!(controller.binding(SECOND), None);
            assert_eq!(controller.binding(REQUESTER), Some(first));
        })
    };
    dma_result(&mut secondary, second_word, SENTINEL);
    prove_fault(&mut controller, SECOND, 2);

    let replacement = unsafe { binding::bind(&mut controller, second_context) };
    assert_ne!(replacement, second);
    assert!(matches!(
        unsafe { controller.begin_retirement(second) },
        Err(BindingRejection::Rejected(Error::Stale))
    ));
    assert_eq!(
        unsafe { controller.begin_refresh(second, scope) },
        Err(BindingRejection::Rejected(Error::Stale))
    );
    dma_result(&mut secondary, second_word, OTHER_PATTERN);
    unsafe {
        binding::retire(&mut controller, replacement, |controller| {
            assert_eq!(controller.observe_change(ticket), BindingProgress::Stale);
        });
        binding::retire(&mut controller, first, |_| {});
    }

    boot_diagnostic::write(
        "PASS vtd-dma requesters=2 translated=2 downgraded=1 unbound=1 stale=1 faults=2\n",
    );
}

pub(super) fn check_firmware(boot: &kernel::entry::boot::Boot<'_>) {
    let acpi = boot.acpi().expect("real firmware ACPI");
    let dmar = acpi.dmar().unwrap().expect("q35 intel-iommu DMAR");
    let mut units = dmar.units();
    let unit = units.next().expect("q35 DRHD");
    assert!(
        units.next().is_none()
            && unit.segment() == 0
            && (unit.includes_all_pci()
                || (unit.lists_root_endpoint(0, 6, 0) && unit.lists_root_endpoint(0, 7, 0)))
    );
    // This board fixture installs the chipset VT-d aperture at this exact
    // identity-mapped UC page. Check firmware against the board before MMIO.
    let address = PhysicalAddress::new(0xfed9_0000);
    assert_eq!(
        unit.registers(),
        address.range(ByteLength::new(4096)).unwrap()
    );
    let mut registers =
        unsafe { DeviceWindow::admit(unit.registers(), InternalAddress::new(address.get())) }
            .unwrap();
    check(unit, &mut registers);
}

//! The q35 EDU endpoint writes a fixture-owned page before translation, translates
//! a distinct IOVA, then faults on a write after the leaf is downgraded.

use super::edu::{Endpoint, REQUESTER};
use core::arch::x86_64::{__cpuid, _mm_clflush, _mm_mfence};
use kernel::entry::boot;
use kernel::metal::{
    memory::mmu::{
        external::vtd::{Controller, Format},
        internal::device::DeviceWindow,
    },
    platform::description::acpi::RemappingUnit,
};
use limen_treaty::model::PhysicalAddress;

const PATTERN: u32 = 0x95a6_3cd1;
const SENTINEL: u32 = !PATTERN;

// Within the EDU DMA mask, outside the 128 MiB guest RAM and all device BARs.
const IOVA: u32 = 0x0de0_0000;

#[repr(C, align(4096))]
struct Table([u64; 512]);

#[repr(C, align(4096))]
struct DataPage([u32; 1024]);

// The major is the sole table writer. Both tables and payload have dedicated
// resident pages and survive all remapping hardware effects through shutdown.
static mut ROOT: Table = Table([0; 512]);
static mut CONTEXT: Table = Table([0; 512]);
static mut LEVEL3: Table = Table([0; 512]);
static mut LEVEL2: Table = Table([0; 512]);
static mut LEVEL1: Table = Table([0; 512]);
static mut PAYLOAD: DataPage = DataPage([0; 1024]);

fn physical<T>(page: *mut T) -> PhysicalAddress {
    // This board fixture retains the OVMF identity mapping of its image.
    PhysicalAddress::new(page.addr() as u64)
}

fn directory(format: Format) -> PhysicalAddress {
    let root = &raw mut ROOT;
    let context = &raw mut CONTEXT;
    let l3 = &raw mut LEVEL3;
    let l2 = &raw mut LEVEL2;
    let l1 = &raw mut LEVEL1;
    // Only bus zero, the EDU requester and the single writable leaf are present.
    unsafe {
        root.cast::<[u64; 2]>()
            .write(format.root_entry(physical(context)).unwrap());
        context
            .cast::<[u64; 2]>()
            .add(usize::from(REQUESTER))
            .write(format.context_entry(physical(l3), 1).unwrap());
        l3.cast::<u64>()
            .add(((IOVA >> 30) & 511) as usize)
            .write(format.table_entry(physical(l2)).unwrap());
        l2.cast::<u64>()
            .add(((IOVA >> 21) & 511) as usize)
            .write(format.table_entry(physical(l1)).unwrap());
        l1.cast::<u64>()
            .add(((IOVA >> 12) & 511) as usize)
            .write(format.writable_leaf(physical(&raw mut PAYLOAD)).unwrap());
    }
    let features = __cpuid(1);
    assert_ne!(features.edx & (1 << 19), 0);
    let line = ((features.ebx >> 8) & 0xff) as usize * 8;
    assert!(line.is_power_of_two());
    for page in [root, context, l3, l2, l1] {
        for offset in (0..4096).step_by(line) {
            unsafe { _mm_clflush(page.cast::<u8>().add(offset)) };
        }
    }
    unsafe { _mm_mfence() };
    physical(root)
}

pub(super) fn check(unit: RemappingUnit<'_>, registers: &mut DeviceWindow) {
    let mut endpoint = Endpoint::open();
    let word = (&raw mut PAYLOAD).cast::<u32>();
    let address = u32::try_from(word.addr()).unwrap();
    assert_ne!(address, IOVA);
    unsafe {
        word.write_volatile(PATTERN);
        _mm_mfence();
    }
    unsafe { endpoint.read_from_ram(address) };
    unsafe {
        word.write_volatile(SENTINEL);
        _mm_mfence();
    }
    unsafe { endpoint.write_to_ram(address) };
    assert_eq!(unsafe { word.read_volatile() }, PATTERN);

    let capability = unsafe { registers.read_u64(0x08) }.unwrap();
    let fault_record = ((capability >> 24) & 0x3ff) as usize * 16;
    assert_eq!(unsafe { registers.read_u32(0x34) }.unwrap() & 3, 0);
    let mut controller = unsafe { Controller::admit(unit, registers) }.unwrap();
    let format = controller
        .format39()
        .expect("q35 39-bit second-level translation");
    let root = directory(format);
    unsafe { controller.install_legacy(root) }.unwrap();
    unsafe {
        word.write_volatile(SENTINEL);
        _mm_mfence();
    }

    unsafe { endpoint.write_to_ram(IOVA) };
    assert_eq!(unsafe { word.read_volatile() }, PATTERN);
    let leaf = unsafe {
        (&raw mut LEVEL1)
            .cast::<u64>()
            .add(((IOVA >> 12) & 511) as usize)
    };
    unsafe {
        leaf.write_volatile(format.read_only_leaf(physical(&raw mut PAYLOAD)).unwrap());
        _mm_clflush(leaf.cast());
        _mm_mfence();
        controller.invalidate_translations().unwrap();
    }
    let mut completion = Err(kernel::metal::memory::mmu::external::vtd::Error::Pending);
    for _ in 0..100_000 {
        completion = controller.observe_invalidation();
        if completion != Err(kernel::metal::memory::mmu::external::vtd::Error::Pending) {
            break;
        }
        core::hint::spin_loop();
    }
    completion.unwrap();
    unsafe {
        word.write_volatile(SENTINEL);
        _mm_mfence();
    }
    unsafe { endpoint.write_to_ram(IOVA) };
    assert_eq!(unsafe { word.read_volatile() }, SENTINEL);
    // A matching primary fault ties the unchanged destination to this write.
    // q35 starts with an empty fault log; PPF and FRI identify its new record.
    let status = unsafe { registers.read_u32(0x34) }.unwrap();
    assert_eq!(status & 3, 2, "one primary fault, no overflow");
    let index = ((status >> 8) & 0xff) as usize;
    let offset = fault_record + 16 * index;
    let info = unsafe { registers.read_u32(offset) }.unwrap();
    let high = unsafe { registers.read_u32(offset + 4) }.unwrap();
    assert_eq!(high, 0);
    assert_eq!(info & !0xfff, IOVA & !0xfff);
    let requester = unsafe { registers.read_u32(offset + 8) }.unwrap();
    assert_eq!(requester as u16, REQUESTER);
    let fault = unsafe { registers.read_u32(offset + 12) }.unwrap();
    assert_ne!(fault & (1 << 31), 0);
    assert_eq!(fault & (1 << 30), 0, "faulting access was a write");
    assert_eq!(fault & 0xff, 5, "second-level page is not writable");
    boot::write(
        "PASS vtd-dma requester=00:06.0 permitted=1 denied=1 translated=1 downgraded=1 fault=1\n",
    );
}

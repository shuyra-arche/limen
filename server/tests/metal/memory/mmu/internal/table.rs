//! Supply and settle the original root in actual boot-admitted RAM.

use core::{
    mem::MaybeUninit,
    ptr::{self, NonNull},
};

use kernel::{
    basis::{
        cell::CellSlot,
        pend::{self, Initial, PendSpace, Revoker},
    },
    entry::boot::Boot,
    metal::{
        memory::{
            access::treaty::ram::MappedRange,
            allocator::treaty::{
                Allocation, AllocationConstruction, Allowance, Error, LookupObservation,
                MappedStorage, RamPurpose, Request, Retired, ReturnReceipt, Search,
            },
            mmu::treaty::{TableError, TableFormat, TableStore},
        },
        sync::treaty::TurnMode::Fast,
    },
};
use limen_treaty::model::{Alignment, ByteLength, PhysicalRange};

static SPENDING: PendSpace = PendSpace::for_page_tables();
static OTHER: PendSpace = PendSpace::for_page_tables();
static FIRST: AllocationConstruction = AllocationConstruction::new();
static SECOND: AllocationConstruction = AllocationConstruction::new();
static CELL: CellSlot = CellSlot::new();
static mut TABLE: MaybeUninit<TableStore> = MaybeUninit::uninit();
static mut RECEIVED: Option<Allocation> = None;
static mut RETIRED: Option<Retired> = None;
static mut RECEIPT: Option<ReturnReceipt> = None;
static mut SPEND: Option<pend::Handle> = None;
static mut CLOSE: Option<Revoker> = None;
static mut OTHER_SPEND: Option<pend::Handle> = None;
static mut OTHER_CLOSE: Option<Revoker> = None;

#[cfg(target_arch = "riscv64")]
fn format(boot: &Boot<'_>) -> TableFormat {
    use kernel::metal::memory::mmu::riscv64::internal::{
        description::{MaximumMode, RiscvMmu},
        format::Format,
    };
    let cpu = boot.riscv_cpus().cpu(boot.hart() as u64).unwrap().unwrap();
    let MaximumMode::Paged(mode) = RiscvMmu::describe(&cpu).unwrap().unwrap().maximum() else {
        panic!("no physical table format on the original Hart");
    };
    assert!(mode >= Format::Sv39);
    TableFormat::Riscv64(mode)
}

#[cfg(target_arch = "aarch64")]
fn format(_: &Boot<'_>) -> TableFormat {
    use core::arch::asm;
    use kernel::metal::memory::mmu::aarch64::internal::{context::Controls, format::Root};
    let original = unsafe { Controls::current() };
    // Qualify the prospective 4 KiB upper root against this PE's actual
    // capabilities without switching the firmware's retained lower root.
    let features: u64;
    unsafe {
        asm!("mrs {}, ID_AA64MMFR0_EL1", out(reg) features, options(nostack, nomem, preserves_flags));
    }
    let candidate = Controls {
        tcr: (original.tcr & !(0xffbf << 16)) | (0xb510 << 16),
        ..original
    };
    TableFormat::Aarch64(
        candidate
            .format(Root::Upper, features)
            .expect("supported EL1 table format"),
    )
}

#[cfg(target_arch = "x86_64")]
fn format(boot: &Boot<'_>) -> TableFormat {
    use kernel::metal::memory::mmu::x86_64::internal::installation::admit_root;
    let (_, format) = unsafe { admit_root(boot.hart()) }.expect("original IA-32e table format");
    TableFormat::X86_64(format)
}

/// The fixture owns the supplied subrange and its live identity view.
pub(super) unsafe fn check(boot: &Boot<'_>, allowance: &Allowance, range: PhysicalRange) {
    assert_eq!(range.length().get(), 4096);
    let table = unsafe { (&mut *(&raw mut TABLE)).write(TableStore::new(format(boot))) };
    let spending = unsafe { &mut *(&raw mut SPEND) };
    let closing = unsafe { &mut *(&raw mut CLOSE) };
    let other_spending = unsafe { &mut *(&raw mut OTHER_SPEND) };
    let other_closing = unsafe { &mut *(&raw mut OTHER_CLOSE) };
    for (space, spending, closing) in [
        (&SPENDING, &mut *spending, &mut *closing),
        (&OTHER, &mut *other_spending, &mut *other_closing),
    ] {
        unsafe {
            space.publish_initial(
                allowance,
                range.length(),
                Alignment::new(4096).unwrap(),
                Initial { spending, closing },
                Fast,
            )
        }
        .unwrap();
    }
    let spending = spending.unwrap();
    let other_spending = other_spending.unwrap();
    let original = allowance.balance().unwrap();
    let allocation = unsafe { &mut *(&raw mut RECEIVED) };
    let retired = unsafe { &mut *(&raw mut RETIRED) };
    let receipt = unsafe { &mut *(&raw mut RECEIPT) };
    let mut previous = None;
    for (construction, handle) in [(&FIRST, spending), (&SECOND, other_spending)] {
        let request = Request::new(
            ByteLength::new(4096),
            Alignment::new(4096).unwrap(),
            range,
            None,
            RamPurpose::PageTable,
        )
        .unwrap();
        let mut observations = [LookupObservation::EMPTY; 1];
        let source = allowance.source();
        let mut search = source.lookup(request, None, &mut observations).unwrap();
        let candidate = loop {
            match search.step().unwrap() {
                Search::Pending => {}
                Search::Candidate(candidate) => break candidate,
                Search::NoRange => panic!("the original private page is free"),
            }
        };
        assert_eq!(candidate.physical(), range);
        let id = handle.reserve(candidate, construction, Fast).unwrap();
        assert_eq!(handle.issue(construction, Fast), Ok(id));
        unsafe { construction.take(allocation) }.unwrap();
        let original_pointer = ptr::with_exposed_provenance_mut(range.start().get() as usize);
        let mut mapping =
            unsafe { MappedRange::from_identity(range, NonNull::new(original_pointer).unwrap()) }
                .unwrap();
        for byte in mapping.bytes() {
            byte.write(0xA5);
        }
        let material = unsafe { MappedStorage::prepare(allocation.take().unwrap(), mapping) }
            .ok()
            .unwrap();
        let node = if previous.is_none() {
            let (material, error) = table
                .publish_cell(&CELL, other_spending, material, Fast)
                .err()
                .unwrap();
            assert_eq!(
                error,
                TableError::Pend(pend::Error::Source(Error::PermitOrigin))
            );
            assert_eq!(material.id(), id);
            table
                .publish_cell(&CELL, handle, material, Fast)
                .ok()
                .unwrap()
        } else {
            table
                .publish_cell(&CELL, handle, material, Fast)
                .ok()
                .unwrap()
        };
        let (cell, physical) = table.cell(node).unwrap();
        assert_eq!(table.locate(id), Ok(Some(node)));
        assert_eq!(physical, range);
        assert_eq!(table.initialize(node, Fast), Err(TableError::State));
        assert_eq!(table.accept(node, Fast), Err(TableError::State));
        table.prepare(node, Fast).unwrap();
        table.prepare(node, Fast).unwrap();
        if let Some((old_node, old_cell, old_allocation)) = previous {
            assert_ne!(node, old_node);
            assert_ne!(cell, old_cell);
            assert_ne!(id, old_allocation);
            assert_eq!(table.locate(old_allocation), Ok(None));
            assert_eq!(table.accept(old_node, Fast), Err(TableError::Scope));
            table.shut(node).unwrap();
            assert_eq!(
                table.accept(node, Fast),
                Err(TableError::Pend(pend::Error::Revoked))
            );
            assert_eq!(table.initialize(node, Fast), Err(TableError::State));
        } else {
            closing.as_ref().unwrap().revoke().unwrap();
            spending.release().unwrap();
            table.accept(node, Fast).unwrap();
            table.accept(node, Fast).unwrap();
            assert_eq!(table.descriptor(node, 0, Fast), Err(TableError::State));
            table.initialize(node, Fast).unwrap();
            assert!((0..512).all(|index| table.descriptor(node, index, Fast) == Ok(0)));
            assert_eq!(table.descriptor(node, 512, Fast), Err(TableError::Layout));
        }
        unsafe { table.retire_private(node, retired, Fast) }.unwrap();
        assert_eq!(table.cell(node), Err(TableError::State));
        assert_eq!(table.locate(id), Ok(None));
        let material = retired.take().unwrap();
        unsafe { material.begin_return(receipt) }.ok().unwrap();
        let original_receipt = receipt.take().unwrap();
        assert_eq!(original_receipt.finish(Fast).unwrap().allocation, id);
        original_receipt.acknowledge().unwrap();
        assert_eq!(allowance.balance(), Ok(original));
        previous = Some((node, cell, id));
    }
    other_closing.as_ref().unwrap().revoke().unwrap();
    other_spending.release().unwrap();
}

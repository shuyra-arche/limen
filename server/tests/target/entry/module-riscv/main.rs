#![no_std]
#![no_main]

mod mapping;

use core::{arch::asm, fmt::Write, mem::MaybeUninit, ops::Range, ptr::NonNull};
use kernel::{
    access::MappedRange,
    address::{PhysicalAddress, PhysicalRange},
    entry::boot::{
        self, Boot, Occupant, RiscvCpus, RiscvExtension, RiscvIsa, RiscvMmu, RiscvMmuMode,
    },
    module::{Abi, Contract, Machine, ModuleImage, Permissions},
};
use limen_treaty::model::PageSize;
use mapping::PAGE;

const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/module.elf"));
const MAX_PAGES: usize = 16;

#[repr(C, align(4096))]
struct Storage([MaybeUninit<u8>; MAX_PAGES * PAGE as usize]);

// The boot image keeps exclusive custody of both arenas through shutdown.
static mut STORAGE: Storage = Storage([MaybeUninit::new(0); MAX_PAGES * PAGE as usize]);

unsafe extern "C" {
    static limen_test_read_only: u8;
    static limen_test_writable: u8;
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(mut console) = boot::console() {
        let _ = writeln!(console, "FAIL riscv-module-execution {info}");
    }
    boot::shutdown(false)
}

fn physical(range: Range<u64>) -> PhysicalRange {
    PhysicalRange::new(
        PhysicalAddress::new(range.start),
        PhysicalAddress::new(range.end),
    )
    .unwrap()
}

fn pages(range: Range<u64>) -> Range<u64> {
    range.start - range.start % PAGE..range.end.checked_next_multiple_of(PAGE).unwrap()
}

fn covers(outer: &Range<u64>, inner: &Range<u64>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

fn check_hart(boot: &Boot<'_>) {
    let tree = boot.device_tree_description().unwrap();
    let cpus = RiscvCpus::new(tree).unwrap().unwrap();
    let cpu = cpus.cpu(boot.hart() as u64).unwrap().unwrap();
    assert!(cpu.enabled());
    let cpu_count = tree
        .nodes()
        .filter(|node| {
            node.parent_id() == Some(cpus.node().id())
                && node.string(b"device_type").unwrap() == Some(b"cpu")
        })
        .count();
    assert_eq!(cpu_count, 1);
    let mmu = RiscvMmu::describe(&cpu).unwrap().unwrap();
    assert!(matches!(
        mmu.maximum(),
        RiscvMmuMode::Sv39 | RiscvMmuMode::Sv48 | RiscvMmuMode::Sv57
    ));
    let isa = RiscvIsa::describe(&cpu).unwrap().unwrap();
    assert!(isa.has(RiscvExtension::InstructionFence));
}

struct Resident {
    text: Range<u64>,
    read_only: Range<u64>,
    writable: Range<u64>,
    tree: Range<u64>,
}

impl Resident {
    fn from_boot(boot: &Boot<'_>, storage: Range<u64>) -> Self {
        let image = boot.image();
        let linked = image.start().get()..image.end().get();
        let read_only = (&raw const limen_test_read_only).addr() as u64;
        let writable = (&raw const limen_test_writable).addr() as u64;
        let tree = boot.device_tree().unwrap();
        let tree = pages(tree.start as u64..tree.end as u64);
        let resident = Self {
            text: linked.start..read_only,
            read_only: read_only..writable,
            writable: writable..pages(linked.clone()).end,
            tree,
        };

        let source = IMAGE.as_ptr_range();
        assert!(covers(
            &resident.read_only,
            &(source.start.addr() as u64..source.end.addr() as u64)
        ));
        let backing = physical(mapping::backing());
        let storage = physical(storage);
        let linked_writable = physical(writable..linked.end);
        assert!(linked_writable.covers(backing));
        assert!(linked_writable.covers(storage));
        assert!(!backing.overlaps(storage));

        let exclusions = boot.exclusions();
        for exclusion in exclusions.entries() {
            if exclusion.occupant == Occupant::Image {
                assert_eq!(exclusion.physical, image);
            } else {
                assert!(!exclusion.physical.overlaps(storage));
                assert!(!exclusion.physical.overlaps(backing));
                if matches!(
                    exclusion.occupant,
                    Occupant::MajorStack | Occupant::TrapStack
                ) {
                    assert!(linked_writable.covers(exclusion.physical));
                }
            }
        }

        let sp: u64;
        let stvec: u64;
        let sscratch: u64;
        unsafe {
            asm!(
                "mv {sp}, sp",
                "csrr {stvec}, stvec",
                "csrr {sscratch}, sscratch",
                sp = out(reg) sp,
                stvec = out(reg) stvec,
                sscratch = out(reg) sscratch,
                options(nomem, nostack),
            );
        }
        assert!(resident.text.contains(&stvec) && stvec.is_multiple_of(4));
        assert!(resident.writable.contains(&sp));
        assert!(sscratch.is_multiple_of(8));
        assert!(covers(&resident.writable, &(sscratch..sscratch + 8)));

        let mapped_image = physical(resident.text.start..resident.writable.end);
        let mapped_tree = physical(resident.tree.clone());
        assert!(!mapped_image.overlaps(mapped_tree));
        let memory = boot.dtb_memory().unwrap();
        for extent in [mapped_image, mapped_tree] {
            assert!(memory.ram().any(|region| region.covers(extent)));
        }
        resident
    }

    fn ranges(&self) -> [(Range<u64>, Permissions); 4] {
        [
            (self.text.clone(), Permissions::ReadExecute),
            (self.read_only.clone(), Permissions::ReadOnly),
            (self.writable.clone(), Permissions::ReadWrite),
            (self.tree.clone(), Permissions::ReadOnly),
        ]
    }

    fn permission(&self, page: u64) -> Option<Permissions> {
        self.ranges()
            .into_iter()
            .find_map(|(range, permission)| range.contains(&page).then_some(permission))
    }

    fn page_count(&self) -> u64 {
        self.ranges()
            .into_iter()
            .map(|(range, _)| (range.end - range.start) / PAGE)
            .sum()
    }
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    check_hart(&boot);
    let image = ModuleImage::select(
        &[IMAGE],
        Machine::Riscv64,
        PageSize::new(PAGE).unwrap(),
        Contract::MmuGeometry,
    )
    .unwrap()
    .unwrap();
    assert_eq!(image.description().abi(), Abi::Scalar);

    let storage = (&raw mut STORAGE).cast::<MaybeUninit<u8>>();
    let start = storage.addr() as u64;
    let length = image.required_length().get();
    assert!(length != 0 && length.is_multiple_of(PAGE));
    let count = length / PAGE;
    assert!(count <= MAX_PAGES as u64);
    assert!(start.is_multiple_of(PAGE));
    let target = start..start + length;
    let resident = Resident::from_boot(&boot, start..start + size_of::<Storage>() as u64);
    let mut root = mapping::Root::new();
    for (range, permission) in resident.ranges() {
        root.map(range, permission);
    }
    let total = resident.page_count();
    root.verify_private(total, |page| resident.permission(page));
    // The live image, retained DTB, and diagnostic entry keep their identity
    // mappings throughout. This virt probe runs one Hart and leaves DMA idle.
    unsafe { root.activate() };
    root.verify_active(total, |page| resident.permission(page));

    let layout = {
        // This image-owned arena has one confirmed RW/NX mapping under Sv39.
        let mut view = unsafe {
            MappedRange::from_identity(physical(target.clone()), NonNull::new(storage).unwrap())
        }
        .unwrap();
        image.prepare(&mut view, start).unwrap()
    };
    assert_eq!(layout.physical(), physical(target.clone()));
    assert_eq!(layout.virtual_range(), target);
    let mut final_pages = [None; MAX_PAGES];
    let final_pages = &mut final_pages[..count as usize];
    for segment in layout.segments() {
        let first = segment.physical.start().get();
        let last = segment.physical.end().get();
        assert_eq!(segment.virtual_range, first..last);
        assert!(covers(&target, &segment.virtual_range));
        assert!(first.is_multiple_of(PAGE) && last.is_multiple_of(PAGE));
        for page in
            &mut final_pages[((first - start) / PAGE) as usize..((last - start) / PAGE) as usize]
        {
            assert!(page.replace(segment.permissions).is_none());
        }
    }
    let entry = layout.entry(Contract::MmuGeometry).unwrap();
    assert!(target.contains(&entry));
    assert_eq!(
        final_pages[((entry - start) / PAGE) as usize],
        Some(Permissions::ReadExecute)
    );
    assert!(final_pages.contains(&Some(Permissions::ReadOnly)));
    assert!(final_pages.contains(&Some(Permissions::ReadWrite)));

    // The write borrow has ended. Revoke every staging translation and
    // complete local invalidation before making any instruction executable.
    root.revoke_write(target.clone());
    root.verify_active(total - count, |page| {
        resident
            .permission(page)
            .filter(|_| !target.contains(&page))
    });
    root.publish(start, final_pages);
    let present = final_pages.iter().filter(|page| page.is_some()).count() as u64;
    root.verify_active(total - count + present, |page| {
        if target.contains(&page) {
            final_pages[((page - start) / PAGE) as usize]
        } else {
            resident.permission(page)
        }
    });

    // The trusted independent fixture declares this scalar ELF entry. Its
    // exact mapping is RX and the publishing Hart has executed FENCE.I.
    let geometry = unsafe { core::mem::transmute::<u64, unsafe extern "C" fn() -> u64>(entry) };
    assert_eq!(unsafe { geometry() }, PAGE);
    boot::write(
        "PASS riscv-module-execution virt sv39=1 private=1 revoked=1 permissions=1 aliases=0 geometry=4096\n",
    );
    boot::shutdown(true)
}

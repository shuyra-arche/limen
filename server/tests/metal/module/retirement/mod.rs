use kernel::metal::platform::boot::treaty as boot_diagnostic;
mod harts;
mod mapping;

use core::{arch::asm, mem::MaybeUninit, ops::Range, ptr::NonNull};
use kernel::{
    entry::boot::{Boot, Occupant},
    metal::{
        memory::access::treaty::ram::MappedRange,
        memory::mmu::{
            riscv64::internal::{
                context::Context, description::RiscvMmu, encoder, format::Format,
                installation::admit_context,
            },
            treaty::{GEOMETRY, LeafAccess, LocalContext},
        },
        module::treaty::{
            Contract, Layout, Machine, ModuleImage, ModuleSlots, SegmentPermissions, UseError,
        },
    },
};
use limen_treaty::model::{ByteLength, InternalAddress, PageSize, PhysicalAddress, PhysicalRange};
use mapping::PAGE;

const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/module.elf"));
const MODES: [Format; 3] = [Format::Sv39, Format::Sv48, Format::Sv57];
const ROOT_IMAGES: [&[u8]; 3] = [
    include_bytes!(concat!(env!("OUT_DIR"), "/sv39-root.elf")),
    include_bytes!(concat!(env!("OUT_DIR"), "/sv48-root.elf")),
    include_bytes!(concat!(env!("OUT_DIR"), "/sv57-root.elf")),
];
const MAX_PAGES: usize = 16;

#[repr(C, align(4096))]
struct Storage([MaybeUninit<u8>; MAX_PAGES * PAGE as usize]);

impl Storage {
    const fn new() -> Self {
        Self([MaybeUninit::new(0); MAX_PAGES * PAGE as usize])
    }
}

// The boot image owns both independently loaded images' exact page reserves.
static mut STORAGE: [Storage; 2] = [const { Storage::new() }; 2];

struct ReservedPages {
    storage: NonNull<MaybeUninit<u8>>,
    physical: PhysicalRange,
}

unsafe extern "C" {
    static limen_test_read_only: u8;
    static limen_test_writable: u8;
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

        for image in core::iter::once(IMAGE).chain(ROOT_IMAGES) {
            let source = image.as_ptr_range();
            assert!(covers(
                &resident.read_only,
                &(source.start.addr() as u64..source.end.addr() as u64)
            ));
        }
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

    fn ranges(&self) -> [(Range<u64>, LeafAccess); 4] {
        [
            (self.text.clone(), LeafAccess::ReadExecute),
            (self.read_only.clone(), LeafAccess::ReadOnly),
            (self.writable.clone(), LeafAccess::ReadWrite),
            (self.tree.clone(), LeafAccess::ReadOnly),
        ]
    }

    fn permission(&self, page: u64) -> Option<LeafAccess> {
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

struct ImagePages {
    range: Range<u64>,
    permissions: [Option<LeafAccess>; MAX_PAGES],
}

impl ImagePages {
    fn count(&self) -> u64 {
        (self.range.end - self.range.start) / PAGE
    }

    fn entries(&self) -> &[Option<LeafAccess>] {
        &self.permissions[..self.count() as usize]
    }

    fn permission(&self, page: u64) -> Option<LeafAccess> {
        self.permissions[((page - self.range.start) / PAGE) as usize]
    }

    fn present(&self) -> u64 {
        self.entries().iter().filter(|page| page.is_some()).count() as u64
    }

    fn restore(&self, root: &mapping::Root, material: ReservedPages, entry: u64) {
        assert_eq!(material.physical, physical(self.range.clone()));
        root.publish(
            self.range.start,
            &[Some(LeafAccess::ReadWrite); MAX_PAGES][..self.count() as usize],
        );
        // Reuse the same physical byte that held the old executable entry.
        let mut reused =
            unsafe { MappedRange::from_identity(material.physical, material.storage) }.unwrap();
        let replaced = &mut reused.bytes()[(entry - self.range.start) as usize];
        unsafe {
            core::ptr::write_volatile(replaced, MaybeUninit::new(0xa5));
            assert_eq!(core::ptr::read_volatile(replaced).assume_init(), 0xa5);
        }
    }
}

fn prepare(
    root_image: &'static [u8],
    contract: Contract<'_>,
    storage: NonNull<MaybeUninit<u8>>,
) -> (Layout<'static>, ReservedPages, ImagePages) {
    let image = ModuleImage::select(
        &[IMAGE, root_image],
        Machine::Riscv64,
        PageSize::new(PAGE).unwrap(),
        contract,
    )
    .unwrap()
    .unwrap();
    let start = storage.addr().get() as u64;
    let length = image.required_length().get();
    assert!(length != 0 && length.is_multiple_of(PAGE));
    assert!(length / PAGE <= MAX_PAGES as u64 && start.is_multiple_of(PAGE));
    let range = start..start.checked_add(length).unwrap();
    let material = ReservedPages {
        storage,
        physical: physical(range.clone()),
    };
    let layout = {
        // This reserved arena has one verified writable, nonexecuting view.
        let mut view =
            unsafe { MappedRange::from_identity(material.physical, material.storage) }.unwrap();
        image
            .prepare(&mut view, InternalAddress::new(start))
            .unwrap()
    };
    assert_eq!(layout.physical(), material.physical);
    assert_eq!(
        layout.internal(),
        InternalAddress::new(start)
            .range(ByteLength::new(length))
            .unwrap()
    );
    let mut permissions = [None; MAX_PAGES];
    for segment in layout.segments() {
        let first = segment.physical.start().get();
        let last = segment.physical.end().get();
        assert_eq!(
            segment.internal,
            InternalAddress::new(first)
                .range(ByteLength::new(last - first))
                .unwrap()
        );
        assert!(layout.internal().covers(segment.internal));
        assert!(first.is_multiple_of(PAGE) && last.is_multiple_of(PAGE));
        for page in
            &mut permissions[((first - start) / PAGE) as usize..((last - start) / PAGE) as usize]
        {
            let access = match segment.permissions {
                SegmentPermissions::ReadOnly => LeafAccess::ReadOnly,
                SegmentPermissions::ReadWrite => LeafAccess::ReadWrite,
                SegmentPermissions::ReadExecute => LeafAccess::ReadExecute,
            };
            assert!(page.replace(access).is_none());
        }
    }
    let entry = layout.entry(contract).unwrap().get();
    let pages = ImagePages { range, permissions };
    assert!(pages.range.contains(&entry));
    assert_eq!(pages.permission(entry), Some(LeafAccess::ReadExecute));
    (layout, material, pages)
}

pub fn check(boot: Boot<'_>) {
    let [major, peer] = harts::check(&boot, &MODES);
    assert!(unsafe { admit_context(peer) }.is_err());
    let mut translation = unsafe { admit_context(major) }.unwrap();
    let service = boot
        .sbi_services()
        .hart_state_management()
        .unwrap()
        .expect("SBI HSM");
    let storage = NonNull::new((&raw mut STORAGE).cast::<Storage>()).unwrap();
    let start = storage.addr().get() as u64;
    let resident = Resident::from_boot(&boot, start..start + size_of::<[Storage; 2]>() as u64);
    for (mode, image) in MODES.into_iter().zip(ROOT_IMAGES) {
        prove_geometry(&mut translation, boot.hart(), mode, &resident);
        exercise(
            &mut translation,
            mode,
            image,
            peer,
            service,
            &resident,
            storage,
        );
    }
    boot_diagnostic::write(
        "PASS riscv-module-execution virt modes=sv39,sv48,sv57 high-leaves=12 geometry=4096 linked=2 roots=2 harts=2 remote-sync=1 remote-typed=2 dependency=1 retired=2 reuse=2\n",
    );
    boot_diagnostic::write("PASS local-context original=1 installed=1 restored=1\n");
}

fn prove_geometry(
    translation: &mut LocalContext<'_>,
    hart: usize,
    mode: Format,
    resident: &Resident,
) {
    static PROBE: u64 = 0x46c8_ce4b_34d7_985e;
    let physical = (&raw const PROBE).addr() as u64;
    assert!(covers(&resident.read_only, &(physical..physical + 8)));

    let mut root = mapping::Root::new(mode);
    for (range, permission) in resident.ranges() {
        root.map(range, permission);
    }
    root.verify_private(resident.page_count(), |page| resident.permission(page));

    let high = u64::MAX << (mode.virtual_bits() - 1);
    let stride = mode.leaf_size(mode.levels() - 1).unwrap().get();
    let mut probes = [0; 5];
    for (level, probe) in probes[..mode.levels()].iter_mut().enumerate() {
        let size = mode.leaf_size(level).unwrap().get();
        let virtual_base = high + level as u64 * stride;
        let offset = physical % size;
        root.map_probe(virtual_base, PhysicalAddress::new(physical - offset), level);
        *probe = virtual_base + offset;
    }

    // The identity mapping is still complete. The minor remains stopped,
    // and neither module has been staged or installed.
    unsafe {
        mapping::probe_context(translation, hart, root.context(), || {
            for &address in &probes[..mode.levels()] {
                let value: u64;
                asm!("ld {value}, 0({address})", value = out(reg) value, address = in(reg) address, options(nostack, readonly));
                assert_eq!(value, PROBE);
            }
        });
        root.clear_inactive();
    }
    root.release();
}

fn exercise(
    translation: &mut LocalContext<'_>,
    mode: Format,
    image: &'static [u8],
    peer: RiscvMmu<'_>,
    service: kernel::metal::platform::firmware::treaty::sbi::HartService,
    resident: &Resident,
    storage: NonNull<Storage>,
) {
    let contract = encoder::contract(mode);
    let mut initial = mapping::Root::new(mode);
    for (range, permission) in resident.ranges() {
        initial.map(range, permission);
    }
    let total = resident.page_count();
    initial.verify_private(total, |page| resident.permission(page));
    // No other Hart is active yet. The independent initial mapping supplies
    // access to both module arenas, both root banks and the trap path.
    unsafe { initial.activate(translation) };
    initial.verify_active(total, |page| resident.permission(page));

    let (geometry_layout, geometry_material, geometry_pages) =
        prepare(image, GEOMETRY, storage.cast());
    let (root_layout, root_material, root_pages) =
        prepare(image, contract, unsafe { storage.add(1) }.cast());
    let geometry_entry = geometry_layout.entry(GEOMETRY).unwrap().get();
    let root_entry = root_layout.entry(contract).unwrap().get();
    let images = [&geometry_pages, &root_pages];
    let reserved = total - geometry_pages.count() - root_pages.count();
    let outside_images = |page| {
        resident
            .permission(page)
            .filter(|_| images.iter().all(|image| !image.range.contains(&page)))
    };
    // Revoke writable staging translations before publishing either image.
    // Only the major can access this initial root at this point.
    for pages in images {
        initial.withdraw(pages.range.clone(), |_| Some(LeafAccess::ReadWrite));
    }
    initial.verify_active(reserved, outside_images);
    for pages in images {
        initial.publish(pages.range.start, pages.entries());
    }
    let published = reserved + geometry_pages.present() + root_pages.present();
    let placed = |page| match images.iter().find(|image| image.range.contains(&page)) {
        Some(image) => image.permission(page),
        None => resident.permission(page),
    };
    initial.verify_active(published, placed);

    let slots = ModuleSlots::<2>::new();
    // The major verified exact permissions and synchronized instructions.
    // The minor will fence before its first use of either root.
    let geometry = unsafe { slots.publish(geometry_layout, geometry_material, []) }
        .ok()
        .unwrap();
    let provider = geometry.acquire(GEOMETRY).unwrap();
    let retained = geometry.acquire(GEOMETRY).unwrap();
    encoder::check(mode, &root_layout, core::slice::from_ref(&retained)).unwrap();
    let backend = unsafe { slots.publish(root_layout, root_material, [retained]) }
        .ok()
        .unwrap();
    let encoder = encoder::bind(mode, &backend).unwrap();
    let mut successor = initial.successor();

    for (range, _) in resident.ranges() {
        for page in range.step_by(PAGE as usize) {
            if let Some(permission) = placed(page) {
                successor.map_with(page..page + PAGE, permission, |address, value| {
                    let address = PhysicalAddress::new(address);
                    unsafe {
                        match value {
                            None => encoder.branch(address),
                            Some(permission) => encoder.leaf(address, 0, permission),
                        }
                    }
                    .unwrap_or_else(|| panic!("{mode:?} encoder rejected {value:?} at {address:?}"))
                });
            }
        }
    }
    successor.verify_inactive(published, placed);
    let root_physical = PhysicalAddress::new(successor.storage().start);
    let context = unsafe { encoder.context(root_physical) }.unwrap();
    assert_eq!(context, successor.context());
    assert_ne!(context, initial.context());
    assert_eq!(context.mode(), Some(mode));
    for level in 0..mode.levels() {
        let boundary = mode.leaf_size(level).unwrap().get();
        let address = PhysicalAddress::new(boundary);
        assert_eq!(
            unsafe { encoder.leaf(address, level, LeafAccess::ReadOnly) },
            Some((boundary >> 2) | 0x43)
        );
    }
    let misaligned = PhysicalAddress::new(root_physical.get() + 1);
    assert!(unsafe { encoder.context(misaligned) }.is_none());

    let replies = unsafe {
        harts::exercise(
            peer,
            service,
            [initial.context(), context],
            &provider,
            &encoder,
            || {
                // The remote Hart still uses the initial root. Switch the major
                // while retaining both table sets and both callable instances.
                mapping::switch_context(translation, initial.context(), context);
                successor.verify_active(published, placed);
                assert_eq!(encoder.context(root_physical).unwrap(), context);
            },
        )
    };
    assert_eq!(replies, [PAGE; 2]);

    // The minor has left both roots and is physically STOPPED. The provider
    // still cannot retire until the dependent executable and its use end.
    geometry.shut();
    assert!(matches!(geometry.acquire(GEOMETRY), Err(UseError::Shut)));
    assert!(matches!(geometry.retire(), Err(UseError::Busy)));
    drop(provider);
    assert!(matches!(geometry.retire(), Err(UseError::Busy)));

    backend.shut();
    assert!(matches!(backend.retire(), Err(UseError::Busy)));
    drop(encoder);
    let retirement = backend.retire().unwrap();
    // No Hart can reenter the old root. Check both roots after withdrawing
    // the dependent's exact pages.
    unsafe {
        initial.withdraw_inactive(root_pages.range.clone(), |page| root_pages.permission(page))
    };
    successor.withdraw(root_pages.range.clone(), |page| root_pages.permission(page));
    let provider_only = |page| placed(page).filter(|_| !root_pages.range.contains(&page));
    initial.verify_inactive(reserved + geometry_pages.present(), provider_only);
    successor.verify_active(reserved + geometry_pages.present(), provider_only);
    unsafe { retirement.complete() };
    assert!(matches!(geometry.retire(), Err(UseError::Busy)));
    let returned_root = backend.reclaim().ok().unwrap();

    let retirement = geometry.retire().unwrap();
    unsafe {
        initial.withdraw_inactive(geometry_pages.range.clone(), |page| {
            geometry_pages.permission(page)
        });
    }
    successor.withdraw(geometry_pages.range.clone(), |page| {
        geometry_pages.permission(page)
    });
    initial.verify_inactive(reserved, outside_images);
    successor.verify_active(reserved, outside_images);
    unsafe { initial.clear_inactive() };
    initial.verify_inactive(0, |_| None);
    unsafe { retirement.complete() };
    let returned_geometry = geometry.reclaim().ok().unwrap();
    root_pages.restore(&successor, returned_root, root_entry);
    geometry_pages.restore(&successor, returned_geometry, geometry_entry);
    successor.verify_active(total, |page| resident.permission(page));
    // Follow the minor's departure with the major's own local completion.
    // The two dormant banks are clear before the next mode can claim them.
    unsafe {
        mapping::switch_context(translation, context, Context::BARE);
        successor.clear_inactive();
    }
    initial.release();
    successor.release();
}

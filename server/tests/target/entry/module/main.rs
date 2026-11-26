#![no_std]
#![no_main]

#[cfg(target_arch = "aarch64")]
#[path = "board/aarch64.rs"]
mod board;
#[cfg(target_arch = "x86_64")]
#[path = "board/x86_64.rs"]
mod board;
#[cfg(target_arch = "aarch64")]
#[path = "arm_mapping.rs"]
mod mapping;
#[cfg(target_arch = "x86_64")]
#[path = "x86_mapping.rs"]
mod mapping;

use core::fmt::{self, Write};
use kernel::{
    access::MappedRange,
    address::{PhysicalAddress, PhysicalRange},
    entry::boot::{self, FirmwareMap},
    module::{Contract, ModuleImage, Permissions},
};
use limen_treaty::model::{ByteLength, PageSize};
use uefi::{
    mem::memory_map::{MemoryAttribute, MemoryMap, MemoryType},
    prelude::*,
    proto::{loaded_image::LoadedImage, security::MemoryProtection},
};

const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/module.elf"));
const MAX_PAGES: usize = 16;
const PAGE: u64 = mapping::PAGE;

#[derive(Clone, Copy)]
struct Placement {
    physical: PhysicalRange,
    pages: [Permissions; MAX_PAGES],
    entry: u64,
    root: mapping::Root,
}

// Written once by the sole UEFI CPU before the nonreturning major handoff.
static mut PLACEMENT: Option<Placement> = None;

struct Output;

impl Write for Output {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if !board::write_byte(byte) {
                return Err(fmt::Error);
            }
        }
        Ok(())
    }
}

static SERVICES: boot::Services = boot::Services {
    output: Some(board::write_byte),
    reset: Some(board::reset),
};

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    let _ = writeln!(Output, "FAIL module-execution {info}");
    board::reset(false);
    loop {
        core::hint::spin_loop();
    }
}

fn physical_range(start: u64, bytes: u64) -> PhysicalRange {
    PhysicalAddress::new(start)
        .range(ByteLength::new(bytes))
        .unwrap()
}

fn checked_map(map: &impl MemoryMap) -> FirmwareMap<'_> {
    let meta = map.meta();
    FirmwareMap::new(map.buffer(), meta.desc_size, meta.desc_version, 512).unwrap()
}

fn prepare() -> Placement {
    let image = ModuleImage::select(
        &[IMAGE],
        board::MACHINE,
        PageSize::new(PAGE).unwrap(),
        Contract::MmuGeometry,
    )
    .unwrap()
    .unwrap();
    let length = image.required_length().get();
    assert!(length != 0 && length.is_multiple_of(PAGE));
    let count = (length / PAGE) as usize;
    assert!(count <= MAX_PAGES);

    let handle = uefi::boot::get_handle_for_protocol::<MemoryProtection>().unwrap();
    let protection = uefi::boot::open_protocol_exclusive::<MemoryProtection>(handle).unwrap();
    let storage = uefi::boot::allocate_pages(
        uefi::boot::AllocateType::AnyPages,
        MemoryType::LOADER_DATA,
        count,
    )
    .unwrap();
    let start = storage.as_ptr().addr() as u64;
    let physical = physical_range(start, length);
    let extent = start..physical.end().get();
    protection
        .set_memory_attributes(extent.clone(), MemoryAttribute::EXECUTE_PROTECT)
        .unwrap();
    assert_eq!(
        protection.get_memory_attributes(extent).unwrap(),
        MemoryAttribute::EXECUTE_PROTECT
    );

    let root = mapping::Root::current().unwrap();
    let before = uefi::boot::memory_map(MemoryType::LOADER_DATA).unwrap();
    let layout = mapping::without_interrupts(|| {
        unsafe { root.check(checked_map(&before), physical, |_| Permissions::ReadWrite) }.unwrap();
        // AllocatePages owns this fresh RAM. The actual mapping now permits
        // one writable NX view, with no device or second CPU using these pages.
        let mut view = unsafe { MappedRange::from_identity(physical, storage.cast()) }.unwrap();
        image.prepare(&mut view, start).unwrap()
    });

    #[cfg(target_arch = "aarch64")]
    mapping::without_interrupts(|| mapping::clean(physical));

    let mut pages = [Permissions::ReadOnly; MAX_PAGES];
    for segment in layout.segments() {
        let first = segment.physical.start().get();
        let last = segment.physical.end().get();
        assert_eq!(segment.virtual_range, first..last);
        pages[((first - start) / PAGE) as usize..((last - start) / PAGE) as usize]
            .fill(segment.permissions);
    }
    let entry = layout.entry(Contract::MmuGeometry).unwrap();
    assert!(entry >= start && entry < physical.end().get());
    assert_eq!(
        pages[((entry - start) / PAGE) as usize],
        Permissions::ReadExecute
    );
    assert!(pages[..count].contains(&Permissions::ReadOnly));
    assert!(pages[..count].contains(&Permissions::ReadWrite));

    for (index, permission) in pages[..count].iter().enumerate() {
        let first = start + index as u64 * PAGE;
        if *permission != Permissions::ReadWrite {
            protection
                .set_memory_attributes(first..first + PAGE, MemoryAttribute::READ_ONLY)
                .unwrap();
        }
    }
    let sealed = uefi::boot::memory_map(MemoryType::LOADER_DATA).unwrap();
    mapping::without_interrupts(|| {
        unsafe {
            root.check(checked_map(&sealed), physical, |page| {
                let index = ((page - start) / PAGE) as usize;
                match pages[index] {
                    Permissions::ReadExecute => Permissions::ReadOnly,
                    permission => permission,
                }
            })
        }
        .unwrap();
    });

    for (index, permission) in pages[..count].iter().copied().enumerate() {
        let first = start + index as u64 * PAGE;
        let scope = first..first + PAGE;
        if permission == Permissions::ReadExecute {
            // The checked root is already read-only and non-executable here.
            protection
                .clear_memory_attributes(scope.clone(), MemoryAttribute::EXECUTE_PROTECT)
                .unwrap();
        }
        let expected = match permission {
            Permissions::ReadOnly => MemoryAttribute::READ_ONLY | MemoryAttribute::EXECUTE_PROTECT,
            Permissions::ReadWrite => MemoryAttribute::EXECUTE_PROTECT,
            Permissions::ReadExecute => MemoryAttribute::READ_ONLY,
        };
        assert_eq!(protection.get_memory_attributes(scope).unwrap(), expected);
    }

    let placement = Placement {
        physical,
        pages,
        entry,
        root,
    };
    let after = uefi::boot::memory_map(MemoryType::LOADER_DATA).unwrap();
    mapping::without_interrupts(|| {
        unsafe {
            root.check(checked_map(&after), physical, |page| {
                placement.permission(page)
            })
        }
        .unwrap();
    });
    placement
}

impl Placement {
    fn permission(&self, page: u64) -> Permissions {
        self.pages[((page - self.physical.start().get()) / PAGE) as usize]
    }
}

#[entry]
fn main() -> Status {
    let device_tree = board::capture();
    let loaded =
        uefi::boot::open_protocol_exclusive::<LoadedImage>(uefi::boot::image_handle()).unwrap();
    let (base, size) = loaded.info();
    let image = physical_range(base.addr() as u64, size);
    drop(loaded);
    let placement = prepare();
    assert!(!image.overlaps(placement.physical));
    unsafe { (&raw mut PLACEMENT).write(Some(placement)) };

    let map = unsafe { uefi::boot::exit_boot_services(None) };
    let meta = map.meta();
    let input = boot::Loader {
        image,
        descriptors: map.buffer(),
        stride: meta.desc_size,
        version: meta.desc_version,
        descriptor_limit: 512,
        device_tree,
        acpi: None,
    };
    unsafe { boot::major(board::hart(), &input, &SERVICES) }
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    let placement = unsafe { (&raw const PLACEMENT).read() }.unwrap();
    let map = boot.firmware_map().unwrap();
    const LOADER_DATA: u32 = 2;
    assert!(map.regions().any(|region| {
        region.kind == LOADER_DATA && region.physical.covers(placement.physical)
    }));

    let geometry = mapping::without_interrupts(|| {
        unsafe {
            placement
                .root
                .check(map, placement.physical, |page| placement.permission(page))
        }
        .unwrap();
        #[cfg(target_arch = "aarch64")]
        mapping::synchronize(placement.physical);
        // The retained pages are executable through the proved initial root.
        // The original trusted image declares the target's scalar geometry ABI.
        unsafe { board::query(placement.entry as usize) }
    });
    assert_eq!(geometry, PAGE);
    writeln!(
        Output,
        "PASS module-execution {} strictnx=1 private=1 permissions=1 identity=1 aliases=0 geometry={geometry}",
        board::NAME
    )
    .unwrap();
    boot::shutdown(true)
}

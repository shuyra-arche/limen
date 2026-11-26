#![no_std]
#![no_main]

mod mapping;

use core::{
    arch::{asm, x86_64::__cpuid},
    fmt::{self, Write},
};
use kernel::{
    access::MappedRange,
    address::{PhysicalAddress, PhysicalRange},
    entry::boot::{self, FirmwareMap},
    module::{Contract, Machine, ModuleImage, Permissions},
};
use limen_treaty::model::{ByteLength, PageSize};
use uefi::{
    mem::memory_map::{MemoryAttribute, MemoryMap, MemoryType},
    prelude::*,
    proto::{loaded_image::LoadedImage, pi::mp::MpServices, security::MemoryProtection},
};

const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/x86_64.elf"));
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
            write_byte(byte);
        }
        Ok(())
    }
}

fn write_byte(byte: u8) -> bool {
    unsafe {
        asm!("out dx, al", in("dx") 0xe9u16, in("al") byte, options(nomem, nostack, preserves_flags))
    };
    true
}

fn reset(success: bool) {
    unsafe {
        asm!("out dx, eax", in("dx") 0xf4u16, in("eax") if success { 0x10u32 } else { 0x11u32 },
        options(nomem, nostack, preserves_flags))
    };
}

static SERVICES: boot::Services = boot::Services {
    output: Some(write_byte),
    reset: Some(reset),
};

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    let _ = writeln!(Output, "FAIL module-execution {info}");
    reset(false);
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

fn assert_single_cpu() {
    let handle = uefi::boot::get_handle_for_protocol::<MpServices>().unwrap();
    let mp = uefi::boot::open_protocol_exclusive::<MpServices>(handle).unwrap();
    let processors = mp.get_number_of_processors().unwrap();
    assert_eq!((processors.total, processors.enabled), (1, 1));
}

fn prepare() -> Placement {
    let image = ModuleImage::select(
        &[IMAGE],
        Machine::X86_64,
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

    for (index, permission) in pages[..count].iter().copied().enumerate() {
        let first = start + index as u64 * PAGE;
        let scope = first..first + PAGE;
        if permission != Permissions::ReadWrite {
            protection
                .set_memory_attributes(scope.clone(), MemoryAttribute::READ_ONLY)
                .unwrap();
        }
        if permission == Permissions::ReadExecute {
            // Close the write mapping while the page is still non-executable.
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
        mapping::synchronize(physical);
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
    assert_single_cpu();
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
        device_tree: None,
        acpi: None,
    };
    let hart = (__cpuid(1).ebx >> 24) as usize;
    unsafe { boot::major(hart, &input, &SERVICES) }
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
        mapping::synchronize(placement.physical);
        // The active, unchanged major root exposes only the verified identity
        // permissions for the retained module pages. The independently built
        // fixture's exact MMU geometry ABI is SysV with a single u64 result.
        let query = unsafe {
            core::mem::transmute::<usize, unsafe extern "sysv64" fn() -> u64>(
                placement.entry as usize,
            )
        };
        unsafe { query() }
    });
    assert_eq!(geometry, PAGE);
    boot::write(
        "PASS module-execution q35 strictnx=1 private=1 permissions=1 identity=1 aliases=0 geometry=4096\n",
    );
    boot::shutdown(true)
}

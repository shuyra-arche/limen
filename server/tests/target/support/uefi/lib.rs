#![no_std]

use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::Minor;

use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::entry::boot;
#[cfg(target_arch = "x86_64")]
use kernel::metal::platform::firmware::uefi::AcpiInput;
use kernel::metal::platform::firmware::uefi::{DeviceTree as LoaderTree, Loader};
use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};
use uefi::{
    Status,
    mem::memory_map::{MemoryMap, MemoryMapRef},
};

static TARGET: AtomicUsize = AtomicUsize::new(usize::MAX);

#[cfg(target_arch = "x86_64")]
pub mod q35;
#[cfg(target_arch = "aarch64")]
#[path = "virt.rs"]
mod virt;
#[cfg(target_arch = "x86_64")]
use q35 as board;
#[cfg(target_arch = "x86_64")]
pub mod interrupt;
#[cfg(target_arch = "aarch64")]
use virt as board;

pub use board::{hart, inspect, inspect_started};

pub fn target() -> usize {
    TARGET.load(Ordering::Acquire)
}

pub fn check_minor(minor: &Minor, hart: usize) {
    assert_eq!(minor.stack_range().len(), boot::stack_range().len());
    assert_eq!(boot::stack_range().len(), 32 * 1024);
    assert_eq!(hart, target());
    assert_eq!(hart, board::hart());
    let sp: usize;
    unsafe {
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!("mov {},sp",out(reg) sp, options(nostack,nomem));
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!("mov {},rsp",out(reg) sp, options(nostack,nomem));
    }
    assert!(
        minor.ready() && minor.stack_range().contains(&sp) && !boot::stack_range().contains(&sp)
    );
}

pub fn load(prepare: impl FnOnce() -> Result<(), Status>) -> Status {
    assert_eq!(
        kernel::metal::sync::treaty::LeafLock::new(()).attempt(|()| ()),
        Err(kernel::metal::sync::treaty::LeafError::Context)
    );
    let loaded = match uefi::boot::open_protocol_exclusive::<uefi::proto::loaded_image::LoadedImage>(
        uefi::boot::image_handle(),
    ) {
        Ok(loaded) => loaded,
        Err(error) => return error.status(),
    };
    let (base, size) = loaded.info();
    drop(loaded);
    let start = PhysicalAddress::new(base.addr() as u64);
    let Some(image) = start.range(ByteLength::new(size)) else {
        return Status::COMPROMISED_DATA;
    };
    let board = match board::capture() {
        Ok(input) => input,
        Err(status) => {
            uefi::println!("Board input: {status:?}");
            return status;
        }
    };
    if let Err(status) = prepare() {
        uefi::println!("Guest preparation: {status:?}");
        return status;
    }
    #[cfg(target_arch = "x86_64")]
    let rsdp = match board::acpi_root() {
        Some(address) => address,
        None => return Status::NOT_FOUND,
    };
    let map = unsafe { uefi::boot::exit_boot_services(None) };
    #[cfg(target_arch = "x86_64")]
    let read_acpi = |physical: PhysicalRange| {
        use uefi::mem::memory_map::MemoryType;
        // This loader keeps ACPI Reclaim and NVS pages identity-mapped and
        // unmodified. Nothing in that window is freed or handed to Limen RAM.
        let start = physical.start().get();
        let end = physical.end().get();
        let source = map.entries().find(|descriptor| {
            matches!(
                descriptor.ty,
                MemoryType::ACPI_RECLAIM | MemoryType::ACPI_NON_VOLATILE
            ) && descriptor.phys_start <= start
                && descriptor
                    .page_count
                    .checked_mul(uefi::boot::PAGE_SIZE as u64)
                    .and_then(|length| descriptor.phys_start.checked_add(length))
                    .is_some_and(|limit| end <= limit)
        })?;
        let length = usize::try_from(end - start).ok()?;
        let address = usize::try_from(start).ok()?;
        if address == 0 || length > isize::MAX as usize || source.virt_start != 0 {
            return None;
        }
        Some(unsafe {
            core::slice::from_raw_parts(core::ptr::with_exposed_provenance(address), length)
        })
    };
    let meta = map.meta();
    let input = Loader {
        image,
        descriptors: map.buffer(),
        stride: meta.desc_size,
        version: meta.desc_version,
        // The selected test configurations admit at most 512 descriptors.
        descriptor_limit: 512,
        device_tree: board.map(LoaderTree::Memory),
        #[cfg(target_arch = "x86_64")]
        acpi: Some(AcpiInput {
            rsdp,
            read: &read_acpi,
        }),
        #[cfg(target_arch = "aarch64")]
        acpi: None,
    };
    unsafe { boot_diagnostic::major(board::hart(), &input, &board::SERVICES) }
}

pub fn extent(start: u64, end: u64) -> PhysicalRange {
    PhysicalRange::new(PhysicalAddress::new(start), PhysicalAddress::new(end)).unwrap()
}

pub fn check_handoff<'a>(boot: &boot::Boot<'a>, minor: Option<&Minor>) -> MemoryMapRef<'a> {
    assert_eq!(boot.hart(), board::hart());
    let map = boot.firmware_map().expect("final UEFI map");
    let early = boot.exclusions();
    let major = boot::stack_range();
    let image = boot.image();
    if let Some(minor) = minor {
        let minor = minor.stack_range();
        assert!(image.covers(extent(minor.start as u64, minor.end as u64)));
    }
    assert!(
        early
            .entries()
            .any(|entry| entry.occupant == boot::Occupant::MajorStack
                && entry.physical == extent(major.start as u64, major.end as u64))
    );
    assert!(
        early.entries().any(
            |entry| entry.occupant == boot::Occupant::TrapStack && image.covers(entry.physical)
        )
    );
    let descriptors = map.descriptors().as_ptr_range();
    assert!(
        early
            .entries()
            .any(|entry| entry.occupant == boot::Occupant::FirmwareMap
                && entry.physical
                    == extent(
                        descriptors.start.addr() as u64,
                        descriptors.end.addr() as u64
                    ))
    );
    if let Some(tree) = boot.device_tree() {
        assert!(
            early
                .entries()
                .any(|entry| entry.occupant == boot::Occupant::DeviceTree
                    && entry.physical == extent(tree.start as u64, tree.end as u64))
        );
    }
    let target = target();
    assert_ne!(target, boot.hart());
    #[cfg(target_arch = "x86_64")]
    board::check_discovery(boot, target);
    check_descriptors(map)
}

fn check_descriptors(
    map: kernel::metal::platform::firmware::treaty::map::FirmwareMap<'_>,
) -> MemoryMapRef<'_> {
    let view = MemoryMapRef::new(
        map.descriptors(),
        uefi::mem::memory_map::MemoryMapMeta {
            map_size: map.descriptors().len(),
            desc_size: map.stride(),
            desc_version: map.version(),
            map_key: Default::default(),
        },
    )
    .unwrap();
    assert_eq!(map.regions().len(), view.entries().count());
    assert!(map.regions().any(|r| r.kind == 7));
    for (region, descriptor) in map.regions().zip(view.entries()) {
        assert_eq!(region.physical.start().get(), descriptor.phys_start);
        assert_eq!(
            region.physical.end().get(),
            descriptor
                .phys_start
                .checked_add(descriptor.page_count.checked_mul(4096).unwrap())
                .unwrap()
        );
        assert_eq!(
            region.kind == 7,
            descriptor.ty == uefi::mem::memory_map::MemoryType::CONVENTIONAL
        );
    }
    view
}

pub fn report_panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    if let Some(mut console) = boot_diagnostic::console() {
        let _ = writeln!(console, "FAIL UEFI platform boot: {info}");
    }
    boot_diagnostic::shutdown(false)
}

#![no_std]
#![no_main]

use core::fmt::Write;
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::{address::PhysicalAddress, entry::boot};
use limen_treaty::model::ByteLength;

#[path = "../foundation.rs"]
mod foundation;
#[path = "../memory.rs"]
mod memory;
mod ram;
#[path = "../synchronization.rs"]
mod synchronization;

static MINOR: boot::Minor = boot::Minor::new();
static TARGET: AtomicUsize = AtomicUsize::new(usize::MAX);
static OBSERVED: AtomicUsize = AtomicUsize::new(usize::MAX);

fn minor_ready(hart: usize, expected: usize) -> ! {
    assert_eq!(MINOR.stack_range().len(), boot::stack_range().len());
    assert_eq!(boot::stack_range().len(), 32 * 1024);
    assert_eq!(hart, expected);
    assert_eq!(hart, board::hart());
    let sp: usize;
    unsafe {
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!("mov {},sp",out(reg) sp, options(nostack,nomem));
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!("mov {},rsp",out(reg) sp, options(nostack,nomem));
    }
    assert!(
        MINOR.ready() && MINOR.stack_range().contains(&sp) && !boot::stack_range().contains(&sp)
    );
    synchronization::check();
    OBSERVED.store(hart, Ordering::Release);
    loop {
        core::hint::spin_loop();
    }
}

use uefi::{
    mem::memory_map::{MemoryMap, MemoryMapRef},
    prelude::*,
};

#[cfg(target_arch = "x86_64")]
#[path = "q35.rs"]
mod board;
#[cfg(target_arch = "aarch64")]
#[path = "virt.rs"]
mod board;

#[entry]
fn main() -> Status {
    assert_eq!(
        kernel::sync::LeafLock::new(()).attempt(|()| ()),
        Err(kernel::sync::LeafError::Context)
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
    if let Err(status) = ram::prepare() {
        uefi::println!("Foundation RAM grant: {status:?}");
        return status;
    }
    let map = unsafe { uefi::boot::exit_boot_services(None) };
    let meta = map.meta();
    let input = boot::Loader {
        image,
        descriptors: map.buffer(),
        stride: meta.desc_size,
        version: meta.desc_version,
        // The selected test configurations admit at most 512 descriptors.
        descriptor_limit: 512,
        device_tree: board.map(boot::DeviceTree::Memory),
    };
    unsafe { boot::major(board::hart(), &input, &board::SERVICES) }
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    assert_eq!(boot.hart(), board::hart());
    let map = boot.firmware_map().expect("final UEFI map");
    let early = boot.exclusions();
    let major = boot::stack_range();
    let minor = MINOR.stack_range();
    let image = boot.image();
    assert!(image.covers(memory::extent(minor.start as u64, minor.end as u64)));
    assert!(
        early
            .entries()
            .any(|entry| entry.occupant == boot::Occupant::MajorStack
                && entry.physical == memory::extent(major.start as u64, major.end as u64))
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
                    == memory::extent(
                        descriptors.start.addr() as u64,
                        descriptors.end.addr() as u64
                    ))
    );
    if let Some(tree) = boot.device_tree() {
        assert!(
            early
                .entries()
                .any(|entry| entry.occupant == boot::Occupant::DeviceTree
                    && entry.physical == memory::extent(tree.start as u64, tree.end as u64))
        );
    }
    let candidate = map
        .regions()
        .find(|entry| entry.kind == 7)
        .expect("conventional memory");
    let scope = candidate.physical;
    assert!(early.retains(scope, None));
    let mut storage = [None; 4];
    let granule = ByteLength::new(uefi::boot::PAGE_SIZE as u64);
    let mut regions = boot.regions(granule, None, &mut storage).unwrap();
    assert!(regions.entries().next().is_none());
    assert!(
        regions
            .reserve_root(&mut None, core::alloc::Layout::new::<u64>())
            .unwrap()
            .is_none()
    );
    let configuration = kernel::allocator::Config {
        granule,
        maximum: ByteLength::new(0),
        regions: 1,
        grains: 1,
        roots: 1,
        allocations: 1,
        returns: 1,
        allowances: 1,
        edits: 2,
    };
    assert!(
        unsafe { regions.allocator(&mut None, configuration, 0, &mut None, &mut []) }
            .unwrap()
            .is_none()
    );
    boot::write(
        "PASS allocator retained without retirement or mapping
",
    );
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
    ram::check(&boot);
    let target = TARGET.load(Ordering::Acquire);
    assert_ne!(target, boot.hart());
    let startup = board::startup();
    assert!(unsafe { MINOR.start(boot.hart(), minor_ready, target, startup) }.is_err());
    unsafe { MINOR.start(target, minor_ready, target, startup) }.expect("minor startup request");
    while OBSERVED.load(Ordering::Acquire) != target {
        core::hint::spin_loop();
    }
    assert!(MINOR.ready() && unsafe { MINOR.start(target, minor_ready, target, startup) }.is_err());
    synchronization::check();
    boot::write("PASS synchronization\n");
    writeln!(
        boot::console().expect("console"),
        "OBSERVED major={} minor={}",
        boot.hart(),
        OBSERVED.load(Ordering::Acquire)
    )
    .unwrap();
    boot::write("PASS early physical exclusions regions=0 roots=0\n");
    board::inspect(boot, &view)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    if let Some(mut console) = boot::console() {
        let _ = writeln!(console, "FAIL UEFI platform boot: {info}");
    }
    boot::shutdown(false)
}

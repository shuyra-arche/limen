#![no_std]
#![no_main]

use core::fmt::Write;
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::entry::boot;

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
    let board = match board::capture() {
        Ok(input) => input,
        Err(status) => {
            uefi::println!("Board input: {status:?}");
            return status;
        }
    };
    let map = unsafe { uefi::boot::exit_boot_services(None) };
    let meta = map.meta();
    let input = boot::Loader {
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
        assert_eq!(region.physical.start, descriptor.phys_start);
        assert_eq!(
            region.physical.end,
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
    let target = TARGET.load(Ordering::Acquire);
    assert_ne!(target, boot.hart());
    let startup = board::startup();
    assert!(unsafe { MINOR.start(boot.hart(), minor_ready, target, startup) }.is_err());
    unsafe { MINOR.start(target, minor_ready, target, startup) }.expect("minor startup request");
    while OBSERVED.load(Ordering::Acquire) != target {
        core::hint::spin_loop();
    }
    assert!(MINOR.ready() && unsafe { MINOR.start(target, minor_ready, target, startup) }.is_err());
    writeln!(
        boot::console().expect("console"),
        "OBSERVED major={} minor={}",
        boot.hart(),
        OBSERVED.load(Ordering::Acquire)
    )
    .unwrap();
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

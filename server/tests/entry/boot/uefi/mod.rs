use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::{Minor, StartError, Startup};
use kernel::metal::{
    processor::treaty::{LocalStorage, Stack},
    sync::treaty::Supply,
};

use kernel::entry::boot;
use uefi::Status;
use uefi_fixture as fixture;

use core::{
    fmt::Write,
    sync::atomic::{AtomicUsize, Ordering},
};
use limen_treaty::model::ByteLength;

#[path = "../foundation.rs"]
mod foundation;
#[path = "../material/memory.rs"]
mod memory;
mod ram;
#[path = "../../../metal/sync/native.rs"]
mod synchronization;
#[path = "../material/window.rs"]
mod window;

static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static OBSERVED: AtomicUsize = AtomicUsize::new(usize::MAX);

fn observe_minor(processor: usize) {
    fixture::check_minor(&MINOR, processor);
    synchronization::check();
    OBSERVED.store(processor, Ordering::Release);
}
fn check_retention(boot: &boot::Boot<'_>) {
    let map = boot.firmware_map().unwrap();
    let early = boot.exclusions();
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
    let configuration = kernel::metal::memory::allocator::treaty::Config {
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
    boot_diagnostic::write("PASS allocator retained without retirement or mapping\n");
}

fn minor(processor: usize, _: usize) -> ! {
    observe_minor(processor);
    loop {
        core::hint::spin_loop();
    }
}

fn start_minor(major: usize, target: usize, startup: &mut Startup) {
    assert_eq!(
        unsafe { MINOR.start(major, minor, 0, startup) },
        Err(StartError::Busy),
    );
    unsafe { MINOR.start(target, minor, 0, startup) }.expect("minor startup");
    while OBSERVED.load(Ordering::Acquire) != target {
        core::hint::spin_loop();
    }
    assert!(MINOR.ready());
    fixture::inspect_started(startup, target);
    assert_eq!(
        unsafe { MINOR.start(target, minor, 0, startup) },
        Err(StartError::Busy),
    );
    finish(major)
}

fn finish(major: usize) {
    synchronization::check();
    boot_diagnostic::write("PASS synchronization\n");
    writeln!(
        boot_diagnostic::console().expect("console"),
        "OBSERVED major={} minor={}",
        major,
        OBSERVED.load(Ordering::Acquire)
    )
    .unwrap();
    boot_diagnostic::write("PASS early physical exclusions regions=0 roots=0\n");
    #[cfg(target_arch = "x86_64")]
    boot_diagnostic::write(
        "PASS q35 major=1 minor=1 ebs=1 cpl0=1 paging=1 masked=1 stack=1 idt=1\n",
    );
    #[cfg(target_arch = "aarch64")]
    boot_diagnostic::write(
        "PASS virt major=1 minor=1 ebs=1 el1=1 masked=1 stack=1 vectors=1 dtb=1 ram=1 reconciled=1\n",
    );
}

pub fn prepare() -> Result<(), Status> {
    // Firmware preparation still precedes kernel local-state admission.
    use kernel::metal::sync::treaty::{LeafError, LeafLock};
    assert_eq!(LeafLock::new(()).attempt(|()| ()), Err(LeafError::Context));
    #[cfg(target_arch = "x86_64")]
    fixture::q35::prepare_minor()?;
    ram::prepare()
}

pub fn check(boot: boot::Boot<'_>, complete: fn() -> !) -> ! {
    let view = fixture::check_handoff(&boot, Some(&MINOR));
    check_retention(&boot);
    let mut startup = fixture::inspect(&boot, &view);
    ram::check(&boot);
    start_minor(boot.processor(), fixture::target(), &mut startup);
    complete()
}

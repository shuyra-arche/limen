#![no_std]
#![no_main]

use kernel::metal::processor::treaty::Minor;
use kernel::metal::{
    processor::treaty::{LocalStorage, Stack},
    sync::treaty::Supply,
};

use kernel::entry::boot;
use uefi::prelude::*;
use uefi_fixture as fixture;

use core::sync::atomic::{AtomicUsize, Ordering};

mod scenario;
#[path = "../../../../metal/sync/native.rs"]
mod synchronization;

static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static OBSERVED: AtomicUsize = AtomicUsize::new(usize::MAX);

fn observe_minor(hart: usize) {
    fixture::check_minor(&MINOR, hart);
    synchronization::check();
    OBSERVED.store(hart, Ordering::Release);
}

#[entry]
fn main() -> Status {
    fixture::load(|| {
        #[cfg(target_arch = "x86_64")]
        fixture::q35::prepare_minor()?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    let view = fixture::check_handoff(&boot, Some(&MINOR));
    let major = boot.hart();
    let target = fixture::target();
    let mut startup = fixture::inspect(&boot, &view);
    scenario::run(boot, major, target, &mut startup, |startup| {
        fixture::inspect_started(startup, target)
    })
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}

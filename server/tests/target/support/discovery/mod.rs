//! T1: the native compiler collects independent cases; startup retains Boot.

use core::{fmt::Write, panic::PanicInfo};
use kernel::metal::platform::boot::treaty as diagnostic;

#[path = "../outcome/mod.rs"]
mod outcome;

#[cfg(target_arch = "riscv64")]
kernel::limen_riscv_major!(stack: 32 * 1024, nodes: 5);

#[cfg(target_os = "uefi")]
#[uefi::entry]
fn main() -> uefi::Status {
    uefi_fixture::load(|| Ok(()))
}

pub fn run(tests: &[&dyn Fn()]) {
    assert!(!tests.is_empty(), "compiler must collect at least one case");
    for test in tests {
        test();
    }
}

pub fn complete() -> ! {
    crate::collected();
    outcome::finish(true)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    if let Some(mut console) = diagnostic::console() {
        let _ = writeln!(console, "{info}");
    }
    outcome::finish(false)
}

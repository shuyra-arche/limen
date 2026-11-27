//! Shared guest entry, machine declaration, and test completion.
#![no_std]

use core::{fmt::Write, panic::PanicInfo};

pub use kernel::entry::boot::Boot;
#[cfg(target_arch = "riscv64")]
pub use kernel::limen_riscv_major as major;
#[cfg(target_os = "uefi")]
pub use uefi_fixture::load;

mod outcome;
pub mod platform;

/// A stateful image has one explicit scenario, not independently collected cases.
pub fn scenario(tests: &[&dyn Fn()]) {
    assert!(tests.is_empty(), "scenario image contains collected cases");
}

/// Independent cases share the admitted initial machine, not mutable test state.
pub fn run(tests: &[&dyn Fn()]) {
    assert!(!tests.is_empty(), "compiler must collect at least one case");
    for test in tests {
        test();
    }
}

pub fn finish() -> ! {
    outcome::finish(true)
}

pub fn fail() -> ! {
    outcome::finish(false)
}

pub fn reject(info: &PanicInfo<'_>) -> ! {
    if let Some(mut console) = kernel::metal::platform::boot::treaty::console() {
        let _ = writeln!(console, "{info}");
    }
    fail()
}

mod entry;

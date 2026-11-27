#![no_std]
#![no_main]

use core::{arch::asm, fmt::Write, panic::PanicInfo};
use kernel::{entry::boot::Boot, metal::platform::boot::treaty as diagnostic};

#[path = "mod.rs"]
mod outcome;

#[cfg(target_arch = "riscv64")]
kernel::limen_riscv_major!(stack: 32 * 1024, nodes: 5);

#[cfg(target_os = "uefi")]
use uefi::prelude::*;

#[cfg(target_os = "uefi")]
#[entry]
fn main() -> Status {
    uefi_fixture::load(|| Ok(()))
}

fn check(boot: Boot<'_>) {
    match option_env!("LIMEN_T0_CASE").unwrap_or("pass") {
        "pass" => assert_ne!(boot.hart(), usize::MAX),
        "panic" => assert_eq!(boot.hart(), usize::MAX, "T0 intentional assertion failure"),
        "shutdown" => diagnostic::shutdown(true),
        "fake-log" => {
            diagnostic::write("PASS\n");
            diagnostic::shutdown(true);
        }
        "trap" => {
            unsafe {
                #[cfg(target_arch = "x86_64")]
                asm!("ud2", options(nostack));
                #[cfg(target_arch = "aarch64")]
                asm!("udf #0", options(nostack));
                #[cfg(target_arch = "riscv64")]
                asm!(".4byte 0", options(nostack));
            }
            panic!("T0 illegal instruction returned");
        }
        "timeout" => loop {
            core::hint::spin_loop();
        },
        _ => panic!("unknown T0 case"),
    }
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    check(boot);
    outcome::finish(true)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    if let Some(mut console) = diagnostic::console() {
        let _ = writeln!(console, "{info}");
    }
    outcome::finish(false)
}

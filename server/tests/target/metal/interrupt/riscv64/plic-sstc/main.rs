#![no_std]
#![no_main]

kernel::limen_riscv_major!(stack: 32 * 1024, nodes: 5);

use core::fmt::Write;
use kernel::entry::boot::Boot;
use kernel::metal::platform::boot::treaty as boot_diagnostic;

#[path = "../../../../../metal/interrupt/riscv64/external/native_plic.rs"]
mod plic;
#[path = "../../../../support/riscv64/reception.rs"]
mod reception;
#[path = "../../../../../metal/interrupt/riscv64/internal/timer/native_sstc.rs"]
mod sstc;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(mut boot: Boot<'_>) -> ! {
    let mut reception = boot.take_reception().unwrap();
    plic::check(&boot, &mut reception);
    sstc::check(&boot, &mut reception.timer);
    boot_diagnostic::shutdown(true);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(mut console) = boot_diagnostic::console() {
        let _ = writeln!(console, "FAIL PLIC and Sstc: {info}");
    }
    boot_diagnostic::shutdown(false);
}

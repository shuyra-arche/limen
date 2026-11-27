#![no_std]
#![no_main]

use core::fmt::Write;
use kernel::entry::boot::{self, Boot};

mod plic;
#[path = "../../../../support/riscv64/reception.rs"]
mod reception;
mod sstc;

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(mut boot: Boot<'_>) -> ! {
    let mut reception = boot.take_reception().unwrap();
    plic::check(&boot, &mut reception);
    sstc::check(&boot, &mut reception.timer);
    boot::shutdown(true);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(mut console) = boot::console() {
        let _ = writeln!(console, "FAIL PLIC and Sstc: {info}");
    }
    boot::shutdown(false);
}

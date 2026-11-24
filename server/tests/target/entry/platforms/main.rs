#![no_std]
#![no_main]

use kernel::entry::boot;
use uefi::{mem::memory_map::MemoryMapOwned, prelude::*};

#[cfg(target_arch = "x86_64")]
#[path = "q35.rs"]
mod board;
#[cfg(target_arch = "aarch64")]
#[path = "virt.rs"]
mod board;

struct Handoff {
    map: MemoryMapOwned,
    board: board::Input,
}

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
    let handoff = Handoff { map, board };
    // Keep the loader stack, final map and firmware data reserved across the
    // switch. The only path to the callback follows successful ExitBootServices.
    unsafe { boot::enter((&raw const handoff) as usize, board::inspect) }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    if let Some(mut console) = boot::console() {
        let _ = writeln!(console, "FAIL UEFI platform boot: {info}");
    }
    boot::shutdown(false)
}

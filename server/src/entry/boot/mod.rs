//! Trusted RV64 firmware handoff before any memory or Unit is admitted.

mod tree;

use core::fmt::{self, Write};
use core::ops::Range;

use crate::metal::riscv::{boot, sbi};

/// Facts from the admitted firmware handoff. These carry no authority over RAM.
pub struct Boot {
    hart: usize,
    device_tree: Range<usize>,
}

impl Boot {
    pub fn hart(&self) -> usize {
        self.hart
    }

    /// Physical address interval occupied by the firmware device tree.
    pub fn device_tree(&self) -> Range<usize> {
        self.device_tree.clone()
    }
}

unsafe extern "Rust" {
    fn limen_platform_ready(boot: Boot) -> !;
}

// OpenSBI transfers control to _start on one boot hart, with placement and
// readable ownership of the device tree guaranteed by the trusted platform.
#[unsafe(no_mangle)]
extern "C" fn limen_boot(hart: usize, device_tree: usize) -> ! {
    if !boot::established() || !sbi::available() {
        write("FAIL Limen boot: invalid supervisor handoff\n");
        shutdown(false);
    }
    let Some(device_tree) = (unsafe { tree::extent(device_tree, boot::image()) }) else {
        write("FAIL Limen boot: invalid device tree header\n");
        shutdown(false);
    };
    unsafe { limen_platform_ready(Boot { hart, device_tree }) }
}

#[unsafe(no_mangle)]
extern "C" fn limen_boot_trap(cause: usize, pc: usize, value: usize, stack: usize) -> ! {
    let _ = writeln!(
        Console,
        "FAIL Limen boot trap: cause={cause:#x} pc={pc:#x} value={value:#x} stack={stack:#x}"
    );
    shutdown(false)
}

/// Write an early diagnostic to the firmware debug console.
pub fn write(message: &str) {
    let _ = Console.write_str(message);
}

/// End the trusted boot path through firmware system reset.
pub fn shutdown(success: bool) -> ! {
    sbi::shutdown(success)
}

struct Console;

impl Write for Console {
    fn write_str(&mut self, message: &str) -> fmt::Result {
        if message.bytes().all(sbi::write_byte) {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

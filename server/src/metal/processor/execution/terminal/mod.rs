//! Terminal output and shutdown from the original platform capabilities.

#[cfg(all(limen_target, any(target_arch = "aarch64", target_arch = "riscv64")))]
use core::arch::asm;
#[cfg(limen_target)]
use core::fmt::{self, Write};

mod console;
pub use console::Console;

#[cfg(limen_target)]
#[derive(Clone, Copy)]
pub struct Terminal {
    output: Option<fn(u8) -> bool>,
    reset: Option<fn(bool)>,
}

#[cfg(limen_target)]
impl Terminal {
    pub(in crate::metal) const fn new(
        output: Option<fn(u8) -> bool>,
        reset: Option<fn(bool)>,
    ) -> Self {
        Self { output, reset }
    }

    pub(in crate::metal) fn console(self) -> Option<Console> {
        self.output.map(Console::new)
    }

    pub(crate) fn reject(self, reason: fmt::Arguments<'_>) -> ! {
        if let Some(mut console) = self.console() {
            let _ = console.write_fmt(reason);
        }
        self.shutdown(false)
    }

    pub(in crate::metal) fn shutdown(self, success: bool) -> ! {
        if let Some(reset) = self.reset {
            reset(success);
        }
        park()
    }
}

#[cfg(all(limen_target, target_arch = "aarch64"))]
fn park() -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

#[cfg(all(limen_target, target_arch = "riscv64"))]
fn park() -> ! {
    loop {
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

#[cfg(all(limen_target, target_arch = "x86_64"))]
#[unsafe(naked)]
extern "sysv64" fn park() -> ! {
    core::arch::naked_asm!("cli", "2: hlt", "jmp 2b");
}

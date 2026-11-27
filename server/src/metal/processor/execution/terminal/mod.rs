//! Terminal output and shutdown from the original platform capabilities.

#[cfg(all(limen_target, any(target_arch = "aarch64", target_arch = "riscv64")))]
use core::arch::asm;
#[cfg(limen_target)]
use core::{
    fmt::{self, Write},
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

#[cfg(all(limen_target, target_arch = "aarch64"))]
pub(in crate::metal) mod aarch64;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub(in crate::metal) mod riscv64;
#[cfg(limen_target)]
pub(crate) mod trap;
#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(in crate::metal) mod x86_64;

mod console;
pub use console::Console;

#[cfg(limen_target)]
#[derive(Clone, Copy)]
pub struct Terminal {
    output: Option<fn(u8) -> bool>,
    reset: Option<fn(bool)>,
}

// The fallback can park even before the image supplies early capabilities.
#[cfg(limen_target)]
static EMPTY: Terminal = Terminal::new(None, None);
#[cfg(limen_target)]
static EARLY: AtomicPtr<Terminal> = AtomicPtr::new(ptr::from_ref(&EMPTY).cast_mut());

#[cfg(limen_target)]
impl Terminal {
    /// Early output and reset operations owned by the trusted image.
    pub const fn new(output: Option<fn(u8) -> bool>, reset: Option<fn(bool)>) -> Self {
        Self { output, reset }
    }

    pub(crate) fn install_early(&'static self) {
        EARLY.store(ptr::from_ref(self).cast_mut(), Ordering::Release);
    }

    pub(crate) fn early() -> Self {
        // Both EMPTY and every installed terminal are immutable statics.
        unsafe { *EARLY.load(Ordering::Acquire) }
    }

    #[cfg(limen_firmware = "sbi")]
    pub(crate) fn sbi_initial() -> Self {
        use crate::metal::platform::firmware::sbi::{extensions, reset, write_byte};

        let available = extensions();
        Self::new(
            available.console.then_some(write_byte),
            available.reset.then_some(reset),
        )
    }

    #[cfg(limen_firmware = "sbi")]
    pub(crate) fn can_report_and_reset(self) -> bool {
        self.output.is_some() && self.reset.is_some()
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

//! RISC-V physical interrupt controllers.

pub mod aplic;
pub mod plic;

/// Ordering for an admitted RV64 I/O mapping. It does not independently
/// prove a device's posted writes have reached its own completion point.
#[derive(Clone, Copy)]
pub struct Barrier {
    private: (),
}

impl Barrier {
    #[cfg(all(limen_target, target_arch = "riscv64"))]
    pub const fn native() -> Self {
        Self { private: () }
    }

    fn synchronize(self) {
        let Self { private: () } = self;
        #[cfg(all(limen_target, target_arch = "riscv64"))]
        unsafe {
            core::arch::asm!("fence iorw, iorw", options(nostack));
        }
        #[cfg(not(all(limen_target, target_arch = "riscv64")))]
        unreachable!("RV64 I/O ordering requires an admitted RV64 target");
    }
}

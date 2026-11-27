//! RISC-V local reception and the exclusive supervisor comparator.

pub mod aclint;
#[cfg(all(limen_target, target_arch = "riscv64", limen_firmware = "sbi"))]
pub mod ipi;
pub mod supervisor;
pub mod timer;

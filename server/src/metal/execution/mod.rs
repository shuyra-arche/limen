//! Actual control-flow transfer for the supported machine backend.

pub mod riscv;

#[cfg(all(limen_target, target_arch = "riscv64"))]
pub(crate) use riscv::switch::{Context, Hart, LeaveReceipt, LocalHart, extensions_off};

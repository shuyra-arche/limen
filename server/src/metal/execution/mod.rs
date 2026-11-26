//! Actual control-flow transfer for the supported machine backend.

pub mod riscv;

#[cfg(limen_cooperative)]
mod cooperative;
#[cfg(limen_cooperative)]
pub(crate) use cooperative::{Hart, LeaveReceipt, LocalHart};
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub(crate) use riscv::switch::{Context, cooperative_ready};

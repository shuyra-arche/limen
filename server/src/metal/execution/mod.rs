//! Actual control-flow transfer for the supported machine backend.
mod riscv;

pub(crate) use riscv::{Context, Hart, LeaveReceipt, LocalHart};

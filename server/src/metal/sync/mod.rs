//! Synchronization for trusted code linked into the kernel image.
//!
//! Lock acquisition is available after the boot entry establishes local
//! protection for the current processor. Synchronous exception entry cannot use it.

mod leaf;
pub(in crate::metal) mod local;
mod snap;
pub mod treaty;
mod turn;

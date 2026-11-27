//! Synchronization for trusted code linked into the kernel image.
//!
//! Lock acquisition is available after the boot entry establishes local
//! protection for the current Hart. Synchronous exception entry cannot use it.

mod leaf;
pub(in crate::metal) mod local;
mod snap;
pub mod treaty;
mod turn;

/// Default node supply selected by the current trusted bootstrap image.
pub const TURN_CAPACITY: usize = 5;

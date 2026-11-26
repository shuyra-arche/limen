//! Physical allocation and translation mechanisms.

#[cfg(any(limen_target, test))]
pub mod allocator;
pub mod mmu;

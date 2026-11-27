//! Physical Hart descriptions and the local context installed at machine entry.

pub mod aarch64;
pub(crate) mod local;
#[cfg(limen_cooperative)]
pub use local::Major;
pub mod riscv64;
pub mod x86_64;

/// A Hart identity used to compare observations in their original scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HartId(usize);

impl HartId {
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

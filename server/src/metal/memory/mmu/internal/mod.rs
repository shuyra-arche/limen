pub mod aarch64;
pub mod access;
pub mod device;
pub mod riscv;
pub mod x86_64;

/// Exact leaf access shared by CPU descriptor encoders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeafAccess {
    ReadOnly,
    ReadWrite,
    ReadExecute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Privilege {
    Kernel,
    User,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Memory {
    Normal,
    Device,
}

/// A control write can only become uncertain after it was issued.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwitchError {
    Context,
    Scope,
    Current,
    Unconfirmed,
}

use crate::metal::module::Contract;

/// Query the active base translation granule using the native integer ABI.
/// The entry takes no arguments and returns the base size in bytes, or zero
/// when the active translation configuration is unsupported.
pub const GEOMETRY: Contract<'static> = Contract::new(b"shuyra,mmu-geometry", 1, 1);

//! Shared CPU translation and I/O binding contracts.

pub use super::table::{TableError, TableFormat, TableId, TableStore};

mod installation;
pub use installation::Installation;
#[cfg(all(
    limen_target,
    any(
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "x86_64"
    )
))]
pub use installation::{Context, LocalContext, LocalInstall};
pub(super) mod binding;
pub use binding::{
    BindingChange, BindingCommand, BindingControl, BindingEffect, BindingObservation,
    BindingProgress, BindingRejection, BindingSubmission,
};

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

use crate::metal::module::treaty::Contract;

/// Query the active base translation granule using the native integer ABI.
/// The entry takes no arguments and returns the base size in bytes, or zero
/// when the active translation configuration is unsupported.
pub const GEOMETRY: Contract<'static> = Contract::new(b"shuyra,mmu-geometry", 1, 1);

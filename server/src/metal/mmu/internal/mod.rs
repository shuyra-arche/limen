pub mod access;
#[cfg(target_arch = "riscv64")]
pub mod riscv;

use crate::module::Contract;

/// Query the active base translation granule using the native integer ABI.
/// The entry takes no arguments and returns the base size in bytes, or zero
/// when the active translation configuration is unsupported.
pub const GEOMETRY: Contract<'static> = Contract::new(b"shuyra,mmu-geometry", 1, 1);

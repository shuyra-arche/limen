//! Admission and typed binding of the Sv39 root encoder.

use crate::metal::{
    mmu::internal::GEOMETRY,
    module::{Contract, Layout, Machine, ModuleUse, UseError},
};

#[cfg(all(limen_target, target_arch = "riscv64"))]
mod native;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub use native::{Sv39Root, bind};

pub const ROOT: Contract<'static> = Contract::new(b"shuyra,sv39-root", 2, 1);

/// Check exact offers and held dependencies while the caller still owns all
/// publication inputs.
pub fn check(layout: &Layout<'_>, dependencies: &[ModuleUse<'_>]) -> Result<(), UseError> {
    if layout.machine() != Machine::Riscv64
        || layout.entry(ROOT).filter(|&entry| entry != 0).is_none()
    {
        return Err(UseError::Interface);
    }
    geometry(dependencies)?;
    Ok(())
}

fn geometry<'borrow, 'provider>(
    dependencies: &'borrow [ModuleUse<'provider>],
) -> Result<&'borrow ModuleUse<'provider>, UseError> {
    let [geometry] = dependencies else {
        return Err(UseError::Interface);
    };
    (geometry.contract() == GEOMETRY
        && geometry.machine() == Machine::Riscv64
        && geometry.entry() != 0)
        .then_some(geometry)
        .ok_or(UseError::Interface)
}

/// The Sv39 root encoding required by the mapping owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u64)]
pub enum Sv39Encoding {
    Branch = 0,
    ReadOnly = 1,
    ReadWrite = 2,
    ReadExecute = 3,
    Context = 4,
}

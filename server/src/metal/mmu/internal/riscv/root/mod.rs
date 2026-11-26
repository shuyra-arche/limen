//! Contracts and admission for RV64 supervisor root encoders.

#[cfg(all(limen_target, target_arch = "riscv64"))]
mod binding;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub use binding::{Root, bind};

use super::Mode;
use crate::metal::{
    mmu::internal::GEOMETRY,
    module::{Contract, Layout, Machine, ModuleUse, UseError},
};

/// Each offer identifies the exact SATP mode the independently linked
/// encoder can produce. All three use the same native v1 calling convention.
pub const fn contract(mode: Mode) -> Contract<'static> {
    let name: &[u8] = match mode {
        Mode::Sv39 => b"shuyra,sv39-root",
        Mode::Sv48 => b"shuyra,sv48-root",
        Mode::Sv57 => b"shuyra,sv57-root",
    };
    Contract::new(name, 2, 1)
}

/// Check exact offers and held dependencies while the caller still owns all
/// publication inputs.
pub fn check(
    mode: Mode,
    layout: &Layout<'_>,
    dependencies: &[ModuleUse<'_>],
) -> Result<(), UseError> {
    if layout.machine() != Machine::Riscv64
        || layout
            .entry(contract(mode))
            .filter(|entry| entry.get() != 0)
            .is_none()
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
        && geometry.entry().get() != 0)
        .then_some(geometry)
        .ok_or(UseError::Interface)
}

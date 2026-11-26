//! Admission and typed binding of the RV64 Sv39 root encoder.

#[cfg(target_arch = "riscv64")]
use core::{mem, num::NonZeroU64};

#[cfg(target_arch = "riscv64")]
use crate::address::PhysicalAddress;
#[cfg(target_arch = "riscv64")]
use crate::module::Module;
use crate::{
    mmu::internal::GEOMETRY,
    module::{Contract, Layout, Machine, ModuleUse, UseError},
};

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

/// Bind an installed encoder to the provider that its module actually retains.
#[cfg(target_arch = "riscv64")]
pub fn bind<'module, Material, const DEPENDENCIES: usize>(
    module: &'module Module<'_, '_, '_, Material, DEPENDENCIES>,
) -> Result<Sv39Root<'module>, UseError> {
    let geometry = geometry(module.dependencies())?;
    let encoder = module.acquire(ROOT)?;
    if encoder.machine() != Machine::Riscv64 || encoder.entry() == 0 {
        return Err(UseError::Interface);
    }
    Ok(Sv39Root::new(encoder, geometry))
}

#[cfg(target_arch = "riscv64")]
type GeometryQuery = unsafe extern "C" fn() -> u64;
#[cfg(target_arch = "riscv64")]
type RootEncoder = unsafe extern "C" fn(*const GeometryV1, u64, u64) -> u64;

/// Geometry V1 is a borrowed input for one encoder invocation. The encoder
/// may call the provider synchronously, but may not retain the table or its
/// pointer after returning.
#[repr(C)]
#[cfg(target_arch = "riscv64")]
struct GeometryV1 {
    version: u32,
    length: u32,
    query: Option<GeometryQuery>,
}

#[cfg(target_arch = "riscv64")]
const _: () = {
    assert!(mem::size_of::<GeometryV1>() == 16);
    assert!(mem::align_of::<GeometryV1>() == 8);
    assert!(mem::offset_of!(GeometryV1, version) == 0);
    assert!(mem::offset_of!(GeometryV1, length) == 4);
    assert!(mem::offset_of!(GeometryV1, query) == 8);
};

/// The Sv39 root encoding required by the mapping owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u64)]
#[cfg(target_arch = "riscv64")]
pub enum Sv39Encoding {
    Branch = 0,
    ReadOnly = 1,
    ReadWrite = 2,
    ReadExecute = 3,
    Context = 4,
}

/// One live encoder bound to the exact geometry dependency in its module.
/// The function pointers are never exposed independently of the use.
#[must_use]
#[cfg(target_arch = "riscv64")]
pub struct Sv39Root<'module> {
    _encoder_use: ModuleUse<'module>,
    _geometry_use: &'module ModuleUse<'module>,
    encoder: RootEncoder,
    geometry: GeometryV1,
}

#[cfg(target_arch = "riscv64")]
impl<'module> Sv39Root<'module> {
    fn new(encoder: ModuleUse<'module>, geometry: &'module ModuleUse<'module>) -> Self {
        // Bind checked both exact declarations and the native machine.
        let query = unsafe { mem::transmute::<u64, GeometryQuery>(geometry.entry()) };
        let entry = unsafe { mem::transmute::<u64, RootEncoder>(encoder.entry()) };
        Self {
            _encoder_use: encoder,
            _geometry_use: geometry,
            encoder: entry,
            geometry: GeometryV1 {
                version: 1,
                length: mem::size_of::<GeometryV1>() as u32,
                query: Some(query),
            },
        }
    }

    /// Encode a physical byte address for this root's branch, leaf, or SATP
    /// context. The caller checks and installs the result in its own root.
    ///
    /// # Safety
    ///
    /// The current Hart and active mapping must be eligible for both live
    /// modules and their synchronous call, including instruction visibility
    /// and an appropriate execution stack.
    pub unsafe fn encode(&self, address: PhysicalAddress, kind: Sv39Encoding) -> Option<u64> {
        let result = unsafe { (self.encoder)(&self.geometry, address.get(), kind as u64) };
        NonZeroU64::new(result).map(NonZeroU64::get)
    }
}

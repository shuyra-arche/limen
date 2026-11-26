//! Native calls through the installed encoder and its held geometry import.

use core::mem;

use limen_treaty::model::PhysicalAddress;
use super::{Mode, contract, geometry};
use crate::metal::{
    mmu::internal::riscv::context::Context,
    module::{Machine, Module, ModuleUse, Permissions, UseError},
};

/// Bind an installed encoder to the provider that its module actually retains.
pub fn bind<'module, Material, const DEPENDENCIES: usize>(
    mode: Mode,
    module: &'module Module<'_, '_, '_, Material, DEPENDENCIES>,
) -> Result<Root<'module>, UseError> {
    let geometry = geometry(module.dependencies())?;
    let encoder = module.acquire(contract(mode))?;
    if encoder.machine() != Machine::Riscv64 || encoder.entry().get() == 0 {
        return Err(UseError::Interface);
    }
    Ok(Root::new(mode, encoder, geometry))
}

type GeometryQuery = unsafe extern "C" fn() -> u64;
type RootEncoder = unsafe extern "C" fn(*const GeometryV1, u64, u64) -> u64;

/// Geometry V1 is a borrowed input for one encoder invocation. The encoder
/// may call the provider synchronously, but may not retain the table or its
/// pointer after returning.
#[repr(C)]
struct GeometryV1 {
    version: u32,
    length: u32,
    query: Option<GeometryQuery>,
}

const _: () = {
    assert!(mem::size_of::<GeometryV1>() == 16);
    assert!(mem::align_of::<GeometryV1>() == 8);
    assert!(mem::offset_of!(GeometryV1, version) == 0);
    assert!(mem::offset_of!(GeometryV1, length) == 4);
    assert!(mem::offset_of!(GeometryV1, query) == 8);
};

/// Operation selectors in the encoder ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u64)]
enum Encoding {
    Branch = 0,
    ReadOnly = 1,
    ReadWrite = 2,
    ReadExecute = 3,
    Context = 4,
}

/// One live encoder bound to the exact geometry dependency in its module.
/// The function pointers are never exposed independently of the use.
#[must_use]
pub struct Root<'module> {
    mode: Mode,
    _encoder_use: ModuleUse<'module>,
    _geometry_use: &'module ModuleUse<'module>,
    encoder: RootEncoder,
    geometry: GeometryV1,
}

impl<'module> Root<'module> {
    fn new(mode: Mode, encoder: ModuleUse<'module>, geometry: &'module ModuleUse<'module>) -> Self {
        // Bind checked both exact declarations and the native machine.
        let query = unsafe { mem::transmute::<u64, GeometryQuery>(geometry.entry().get()) };
        let entry = unsafe { mem::transmute::<u64, RootEncoder>(encoder.entry().get()) };
        Self {
            mode,
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

    /// Encode the ASID-zero SATP context for the bound mode.
    ///
    /// # Safety
    ///
    /// The active mapping and current Hart must permit calling both live
    /// modules with a suitable stack and instruction visibility.
    pub unsafe fn context(&self, address: PhysicalAddress) -> Option<Context> {
        let context = Context::paged(self.mode, address)?;
        (unsafe { self.encode(address, Encoding::Context) } == context.bits()).then_some(context)
    }

    /// Encode a branch into a base table page.
    ///
    /// # Safety
    ///
    /// The call requires the same mapping and Hart eligibility as [Self::context].
    pub unsafe fn branch(&self, address: PhysicalAddress) -> Option<u64> {
        let expected = Mode::branch(address)?;
        (unsafe { self.encode(address, Encoding::Branch) } == expected).then_some(expected)
    }

    /// Encode an aligned leaf at a level supported by the bound mode.
    ///
    /// # Safety
    ///
    /// The call requires the same mapping and Hart eligibility as [Self::context].
    pub unsafe fn leaf(
        &self,
        address: PhysicalAddress,
        level: usize,
        permission: Permissions,
    ) -> Option<u64> {
        let expected = self.mode.leaf(address, level, permission)?;
        let kind = match permission {
            Permissions::ReadOnly => Encoding::ReadOnly,
            Permissions::ReadWrite => Encoding::ReadWrite,
            Permissions::ReadExecute => Encoding::ReadExecute,
        };
        (unsafe { self.encode(address, kind) } == expected).then_some(expected)
    }

    unsafe fn encode(&self, address: PhysicalAddress, kind: Encoding) -> u64 {
        unsafe { (self.encoder)(&self.geometry, address.get(), kind as u64) }
    }
}

//! Typed binding of the RV64 Sv39 root encoder.

use core::{mem, num::NonZeroU64};

use super::ModuleUse;
use crate::address::PhysicalAddress;

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

/// One live encoder bound to the exact geometry dependency in its module.
/// The function pointers are never exposed independently of the use.
#[must_use]
pub struct Sv39Root<'module> {
    _encoder_use: ModuleUse<'module>,
    _geometry_use: &'module ModuleUse<'module>,
    encoder: RootEncoder,
    geometry: GeometryV1,
}

impl<'module> Sv39Root<'module> {
    pub(super) fn new(encoder: ModuleUse<'module>, geometry: &'module ModuleUse<'module>) -> Self {
        // Module::bind_sv39 checks the declarations of both retained,
        // published modules for their exact native ABI and version.
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

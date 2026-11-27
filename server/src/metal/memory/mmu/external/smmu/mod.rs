//! Arm SMMUv3 non-secure stage-2 capability and disabled-interface policy.

use crate::metal::{
    memory::mmu::internal::device::DeviceWindow,
    platform::description::devicetree::{Error as TreeError, Node},
};
use limen_treaty::model::PhysicalRange;

const IDR0: usize = 0;
const IDR5: usize = 0x14;
const CR0: usize = 0x20;
const CR0_ACK: usize = 0x24;
const GBPA: usize = 0x44;
const SMMU_ENABLE: u32 = 1;
const ABORT: u32 = 1 << 20;
const UPDATE: u32 = 1 << 31;

pub fn registers(node: Node<'_, '_>) -> Result<Option<PhysicalRange>, TreeError> {
    if !node.enabled()? || !node.compatible(b"arm,smmu-v3")? {
        return Ok(None);
    }
    if node.property(b"#iommu-cells")? != Some(&1u32.to_be_bytes()) {
        return Err(TreeError::Unsupported);
    }
    Ok(Some(node.register(0)?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Window,
    Busy,
    Active,
    Rejected,
}

#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub stage_two: bool,
    pub granule_4k: bool,
    pub enabled: bool,
}

unsafe fn read(window: &mut DeviceWindow, offset: usize) -> Result<u32, Error> {
    unsafe { window.read_u32(offset) }
        .map(u32::from_le)
        .map_err(|_| Error::Window)
}

unsafe fn enabled(window: &mut DeviceWindow) -> Result<bool, Error> {
    let requested = unsafe { read(window, CR0)? } & SMMU_ENABLE;
    let observed = unsafe { read(window, CR0_ACK)? } & SMMU_ENABLE;
    if requested != observed {
        return Err(Error::Busy);
    }
    Ok(observed != 0)
}

/// # Safety
/// The mapped window must cover the actual Non-secure, little-endian SMMUv3
/// registers and exclude concurrent software programming and reset.
pub unsafe fn probe(window: &mut DeviceWindow) -> Result<Profile, Error> {
    let idr0 = unsafe { read(window, IDR0)? };
    let idr5 = unsafe { read(window, IDR5)? };
    Ok(Profile {
        stage_two: idr0 & 1 != 0,
        granule_4k: idr5 & (1 << 4) != 0,
        enabled: unsafe { enabled(window)? },
    })
}

#[must_use]
pub struct Abort<'a> {
    window: &'a mut DeviceWindow,
    order: fn(),
}

impl Abort<'_> {
    /// Confirms GBPA.ABORT while SMMUEN is clear. This does not retire a
    /// previous STE, ATC entry or transaction.
    pub fn observe(&mut self) -> Result<(), Error> {
        (self.order)();
        if unsafe { enabled(self.window)? } {
            return Err(Error::Active);
        }
        let policy = unsafe { read(self.window, GBPA)? };
        if policy & UPDATE != 0 {
            return Err(Error::Busy);
        }
        if policy & ABORT == 0 {
            return Err(Error::Rejected);
        }
        Ok(())
    }
}

/// Program the disabled interface to abort transactions. Does not disable
/// the SMMU or change an enabled stream table.
///
/// # Safety
/// Satisfy `probe`'s register and exclusive-control contract, own the policy
/// for all affected streams, and supply the platform device-ordering barrier.
/// Retain control and in-flight obligations if acknowledgement is delayed.
pub unsafe fn abort_bypass(window: &mut DeviceWindow, order: fn()) -> Result<Abort<'_>, Error> {
    if unsafe { enabled(window)? } {
        return Err(Error::Active);
    }
    if unsafe { read(window, GBPA)? } & UPDATE != 0 {
        return Err(Error::Busy);
    }
    order();
    unsafe { window.write_u32(GBPA, (UPDATE | ABORT).to_le()) }.map_err(|_| Error::Window)?;
    Ok(Abort { window, order })
}

pub mod queue;

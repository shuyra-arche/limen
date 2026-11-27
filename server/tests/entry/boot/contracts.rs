#![no_std]

use kernel::{basis::pend::{self, Revoker}, metal::{memory::allocator::{self, AllocationConstruction}, sync::TurnMode}};

pub fn cancel(receiver: &'static AllocationConstruction) -> Result<(), allocator::Error> {
    receiver.shut()?;
    receiver.cancel(TurnMode::Fast)
}

pub fn revoke(revoker: &Revoker) -> Result<bool, pend::Error> {
    revoker.revoke()?;
    revoker.revoked()
}

#[cfg(contract = "receiver")]
pub fn raw_receiver(receiver: &'static AllocationConstruction) {
    let _ = &receiver.state;
}

#[cfg(contract = "role")]
pub fn raw_space(revoker: &Revoker) {
    let _ = &revoker.space;
}

#[cfg(contract = "mapping")]
pub fn raw_mapping(mapping: &kernel::metal::memory::mmu::internal::access::MappedRange<'_>) {
    let _ = &mapping.bytes;
}

#[cfg(contract = "material")]
pub fn raw_material(material: &kernel::metal::memory::allocator::MappedStorage<'_>) {
    let _ = &material.allocation;
}

#[cfg(contract = "mapping-move")]
pub fn reuse_mapping(mapping: kernel::metal::memory::mmu::internal::access::MappedRange<'_>) {
    let physical = mapping.physical();
    let offset = physical.length();
    let _ = mapping.split(offset);
    let _ = mapping.physical();
}

#[cfg(contract = "material-move")]
pub fn reuse_material(material: kernel::metal::memory::allocator::MappedStorage<'_>) {
    let _ = material.into_parts();
    let _ = material.id();
}

#[cfg(contract = "grant")]
pub fn raw_grant(source: &'static allocator::Allocator, description: allocator::RegionDescription) {
    let _ = unsafe { allocator::Grant::from_retired(source, description) };
}

#[cfg(contract = "return-authority")]
pub fn acknowledge_discovered_return(source: &allocator::Allocator, id: allocator::ReturnId) {
    let _ = source.acknowledge(id);
}

#[cfg(contract = "return-clone")]
pub fn duplicate_receipt(receipt: &allocator::ReturnReceipt) {
    let _ = Clone::clone(receipt);
}

#[cfg(contract = "return-construction")]
pub fn manufacture_receipt(source: &'static allocator::Allocator, id: allocator::ReturnId) {
    let _ = allocator::ReturnReceipt { source, id };
}

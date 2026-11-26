#![no_std]

use kernel::{
    allocator::{self, Construction},
    pend::{self, Revoker},
    sync::Acquisition,
};

pub fn cancel(receiver: &'static Construction) -> Result<(), allocator::Error> {
    receiver.close()?;
    receiver.cancel(Acquisition::Fast)
}

pub fn revoke(revoker: &Revoker) -> Result<bool, pend::Error> {
    revoker.revoke()?;
    revoker.revoked()
}

#[cfg(contract = "receiver")]
pub fn raw_receiver(receiver: &'static Construction) {
    let _ = &receiver.state;
}

#[cfg(contract = "role")]
pub fn raw_space(revoker: &Revoker) {
    let _ = &revoker.space;
}

#[cfg(contract = "mapping")]
pub fn raw_mapping(mapping: &kernel::access::MappedRange<'_>) {
    let _ = &mapping.bytes;
}

#[cfg(contract = "material")]
pub fn raw_material(material: &kernel::allocator::MappedStorage<'_>) {
    let _ = &material.allocation;
}

#[cfg(contract = "mapping-move")]
pub fn reuse_mapping(mapping: kernel::access::MappedRange<'_>) {
    let _ = mapping.split(1);
    let _ = mapping.physical();
}

#[cfg(contract = "material-move")]
pub fn reuse_material(material: kernel::allocator::MappedStorage<'_>) {
    let _ = material.into_parts();
    let _ = material.id();
}

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

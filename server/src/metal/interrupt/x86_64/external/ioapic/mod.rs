//! Legacy fixed-vector routing through one exclusive physical I/O APIC.

use crate::metal::{
    memory::access::treaty::device::DeviceWindow,
    platform::description::acpi::{IoApic, Madt},
};

mod pin;
mod port;
pub mod route;

use pin::{PIN_LIMIT, Pins};
use port::Port;
use route::Binding;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Unconfirmed,
}

/// One actual port and its per-pin programming obligations. Dropping the
/// object neither disables hardware nor proves drain.
#[must_use]
pub struct Controller<'a> {
    port: Port<'a>,
    madt: Madt<'a>,
    pins: Pins,
    routes: [Option<Binding>; PIN_LIMIT],
}

impl<'a> Controller<'a> {
    /// # Safety
    /// The window is this physical controller's live, UC, x86 mapping. The
    /// caller retains exclusive access to its selector and data pair across
    /// CPUs and interrupt contexts, including failures. Direct compatibility
    /// format is active, interrupt remapping cannot reinterpret these entries,
    /// and this controller's probed GSI interval is exclusive in the system.
    /// Keep this controller live until every reserved route and effect settles.
    pub unsafe fn admit(madt: Madt<'a>, window: &'a mut DeviceWindow<'_>) -> Result<Self, Failure> {
        let address = window.physical().start();
        let IoApic { id, gsi_base, .. } = madt
            .io_apics()
            .find(|item| item.address == address)
            .ok_or(Failure::Scope)?;
        let mut port = Port::from_window(window).ok_or(Failure::Scope)?;
        if id > 15 || (port.read(0) >> 24) != u32::from(id) {
            return Err(Failure::Scope);
        }
        let pins = Pins::probe(gsi_base, port.read(1)).ok_or(Failure::Scope)?;
        if madt
            .io_apics()
            .any(|item| item.id != id && pins.pin(item.gsi_base).is_ok())
        {
            return Err(Failure::Scope);
        }
        Ok(Self {
            port,
            madt,
            pins,
            routes: [const { None }; PIN_LIMIT],
        })
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/x86_64/external/ioapic/mod.rs"]
mod tests;

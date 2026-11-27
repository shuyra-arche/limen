use core::{cell::UnsafeCell, sync::atomic::AtomicBool};

use super::local;

/// Failure to enter a local leaf section or obtain its exclusive access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeafError {
    Busy,
    Context,
    Capacity,
}

impl From<local::Error> for LeafError {
    fn from(error: local::Error) -> Self {
        match error {
            local::Error::Context => Self::Context,
            local::Error::Capacity => Self::Capacity,
        }
    }
}

/// Exclusive access to one local hardware conflict domain or snapshot writer.
///
/// A leaf section must do finite, prepared work without waiting for hardware,
/// allocating, acquiring another lock, or calling a domain service. A hardware
/// owner supplies any device barriers and actual completion evidence.
pub struct LeafLock<T> {
    pub(super) occupied: AtomicBool,
    pub(super) value: UnsafeCell<T>,
}

// A successful acquire CAS and the matching release are the sole path to the
// value. Moving exclusive access between Harts requires T: Send.
unsafe impl<T: Send> Sync for LeafLock<T> {}

impl<T> LeafLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            occupied: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }
}

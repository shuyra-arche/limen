use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{
        AtomicBool,
        Ordering::{Acquire, Relaxed, Release},
    },
};

use crate::metal::hart::{self, LeafParticipation};

/// Failure to enter a local leaf section or obtain its exclusive access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeafError {
    Busy,
    Context,
    Capacity,
}

impl From<hart::Error> for LeafError {
    fn from(error: hart::Error) -> Self {
        match error {
            hart::Error::Context => Self::Context,
            hart::Error::Capacity => Self::Capacity,
        }
    }
}

/// Exclusive access to one local hardware conflict domain or snapshot writer.
///
/// A leaf section must do finite, prepared work without waiting for hardware,
/// allocating, acquiring another lock, or calling a domain service. A hardware
/// owner supplies any device barriers and actual completion evidence.
pub struct LeafLock<T> {
    occupied: AtomicBool,
    value: UnsafeCell<T>,
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

    /// Tries once after checking the actual Hart-local entry conditions.
    ///
    /// From a complete Turn section, try before any irreversible effect that
    /// relies on obtaining this leaf. A failed try never invokes the callback.
    pub fn attempt<R>(
        &self,
        operation: impl for<'value> FnOnce(&'value mut T) -> R,
    ) -> Result<R, LeafError> {
        self.with(false, operation)
    }

    /// Waits for this leaf in an ordinary context with no lock participation.
    pub fn acquire<R>(
        &self,
        operation: impl for<'value> FnOnce(&'value mut T) -> R,
    ) -> Result<R, LeafError> {
        self.with(true, operation)
    }

    fn with<R>(
        &self,
        wait: bool,
        operation: impl for<'value> FnOnce(&'value mut T) -> R,
    ) -> Result<R, LeafError> {
        let participation = hart::begin_leaf(&raw const self.occupied, wait)?;
        loop {
            if self
                .occupied
                .compare_exchange(false, true, Acquire, Relaxed)
                .is_ok()
            {
                break;
            }
            if !wait {
                return Err(LeafError::Busy);
            }
            while self.occupied.load(Relaxed) {
                spin_loop();
            }
        }

        participation.held();
        let held = Held {
            occupied: &self.occupied,
            participation,
        };
        // The successful CAS grants exclusive access until Held drops. The
        // callback's higher-ranked argument prevents this borrow from escaping.
        let result = operation(unsafe { &mut *self.value.get() });
        drop(held);
        Ok(result)
    }
}

struct Held<'lock> {
    occupied: &'lock AtomicBool,
    participation: LeafParticipation,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.participation.releasing();
        self.occupied.store(false, Release);
        // Dropping participation after the store restores the local prefix.
    }
}

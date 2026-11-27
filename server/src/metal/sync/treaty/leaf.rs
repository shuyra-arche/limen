use core::{
    hint::spin_loop,
    mem::DropGuard,
    sync::atomic::Ordering::{Acquire, Relaxed, Release},
};

use crate::metal::sync::{
    leaf::{LeafError, LeafLock},
    local,
};

impl<T> LeafLock<T> {
    /// Tries once after checking the actual processor-local entry conditions.
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
        let participation = local::begin_leaf(&raw const self.occupied, wait)?;
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

        let held = DropGuard::new(participation, |participation| {
            participation.releasing();
            self.occupied.store(false, Release);
            // Dropping participation restores the local prefix.
        });
        held.held();
        // The successful CAS grants exclusive access through this call.
        // The callback's higher-ranked argument keeps the borrow scoped.
        let result = operation(unsafe { &mut *self.value.get() });
        drop(held);
        Ok(result)
    }
}

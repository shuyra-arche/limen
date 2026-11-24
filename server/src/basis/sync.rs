use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{
        AtomicBool,
        Ordering::{Acquire, Relaxed, Release},
    },
};

/// Short protection for execution publication while local interrupts are masked.
pub(crate) struct Lock<T> {
    held: AtomicBool,
    value: UnsafeCell<T>,
}

// Every access to the value requires the single acquired guard.
unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            held: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub(crate) fn try_lock(&self) -> Option<Guard<'_, T>> {
        self.held
            .compare_exchange(false, true, Acquire, Relaxed)
            .ok()?;
        Some(Guard {
            lock: self,
            local: PhantomData,
        })
    }

    /// Used by trusted entry and terminal paths with no other guard held.
    pub(crate) fn lock(&self) -> Guard<'_, T> {
        loop {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            core::hint::spin_loop();
        }
    }
}

pub(crate) struct Guard<'a, T> {
    lock: &'a Lock<T>,
    local: PhantomData<*mut ()>,
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.lock.held.store(false, Release);
    }
}

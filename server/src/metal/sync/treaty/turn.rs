use core::{marker::PhantomData, ptr};

use crate::metal::sync::turn::{
    TurnAccess, TurnAcquireError, TurnLock, TurnMode, TurnPlan, TurnPlanError, TurnWorkspace,
    acquire, member,
};

impl<T> TurnLock<T> {
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    /// Queue for this lock and borrow its value for one ordinary operation.
    pub fn acquire<R>(
        &self,
        operation: impl for<'held> FnOnce(&'held mut T) -> R,
    ) -> Result<R, TurnAcquireError> {
        acquire(&[Some(&self.queue)], TurnMode::Fast, || {
            // The queue is exclusively held until the scoped callback ends.
            operation(unsafe { &mut *self.value.get() })
        })
    }
}

impl<'object, const N: usize> TurnWorkspace<'object, N> {
    pub const fn new() -> Self {
        Self { queues: [None; N] }
    }

    pub fn plan(&mut self) -> TurnPlan<'object, '_> {
        TurnPlan {
            queues: &mut self.queues,
            length: 0,
            overflowed: false,
        }
    }
}

impl<const N: usize> Default for TurnWorkspace<'_, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'object> TurnPlan<'object, '_> {
    /// A physical lock that is already present retains its original position.
    pub fn insert<T>(&mut self, lock: &'object TurnLock<T>) -> Result<(), TurnPlanError> {
        if self.queues[..self.length]
            .iter()
            .any(|queue| ptr::eq(member(*queue), &lock.queue))
        {
            return Ok(());
        }
        let Some(slot) = self.queues.get_mut(self.length) else {
            self.overflowed = true;
            return Err(TurnPlanError::Capacity);
        };
        *slot = Some(&lock.queue);
        self.length += 1;
        Ok(())
    }

    /// Run `operation` only after acquiring the entire set. On Busy, all
    /// acquired locks and the original local interrupt state are settled.
    ///
    /// An operation must finish its shared-state changes before returning and
    /// move work that may wait or reacquire a Turn to the outside of the closure.
    pub fn acquire<R>(
        self,
        mode: TurnMode,
        operation: impl for<'held> FnOnce(&mut TurnAccess<'held>) -> R,
    ) -> Result<R, TurnAcquireError> {
        if self.overflowed {
            return Err(TurnAcquireError::Capacity);
        }
        if self.length == 0 {
            return Err(TurnAcquireError::Empty);
        }
        let queues = &mut self.queues[..self.length];
        if mode == TurnMode::Slow {
            queues.sort_unstable_by_key(|queue| ptr::from_ref(member(*queue)).addr());
        }
        acquire(queues, mode, || {
            operation(&mut TurnAccess {
                queues,
                local: PhantomData,
            })
        })
    }
}

impl TurnAccess<'_> {
    fn contains<T>(&self, lock: &TurnLock<T>) -> bool {
        self.queues
            .iter()
            .any(|queue| ptr::eq(member(*queue), &lock.queue))
    }

    pub fn access<'borrow, T>(
        &'borrow mut self,
        lock: &'borrow TurnLock<T>,
    ) -> Option<&'borrow mut T> {
        if self.contains(lock) {
            // The private owner holds this exact queue, and the &mut borrow
            // prevents simultaneous accesses through the set.
            Some(unsafe { &mut *lock.value.get() })
        } else {
            None
        }
    }

    /// Borrow two distinct, already held locks together for one commit.
    /// Neither borrow can outlive this access to the complete set.
    pub fn access_pair<'borrow, T, U>(
        &'borrow mut self,
        first: &'borrow TurnLock<T>,
        second: &'borrow TurnLock<U>,
    ) -> Option<(&'borrow mut T, &'borrow mut U)> {
        if ptr::eq(&first.queue, &second.queue) || !self.contains(first) || !self.contains(second) {
            return None;
        }
        // Both queues are held by this set, and they guard distinct values.
        // The exclusive borrow of the set prevents any other live access.
        unsafe { Some((&mut *first.value.get(), &mut *second.value.get())) }
    }
}

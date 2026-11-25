use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    ptr::{self, null_mut},
    sync::atomic::Ordering::Relaxed,
};

use crate::metal::hart::{self, TurnParticipation};

use super::queue::Queue;

/// FIFO protection for ordinary shared state, acquired through a complete plan.
///
/// The lock must have one canonical address for as long as any reference
/// exists. Placing it behind another virtual alias requires external exclusion
/// of all simultaneous access through that alias.
pub struct TurnLock<T> {
    queue: Queue,
    value: UnsafeCell<T>,
}

// The MCS handoff transfers exclusive access to T to the next owner.
unsafe impl<T: Send> Sync for TurnLock<T> {}

impl<T> TurnLock<T> {
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    pub const fn new(value: T) -> Self {
        Self {
            queue: Queue::new(),
            value: UnsafeCell::new(value),
        }
    }
}

/// Caller-owned storage for the unique physical locks of one acquisition.
pub struct Work<'object, const N: usize> {
    queues: [Option<&'object Queue>; N],
}

impl<'object, const N: usize> Work<'object, N> {
    pub const fn new() -> Self {
        Self { queues: [None; N] }
    }

    pub fn plan(&mut self) -> LockPlan<'object, '_> {
        LockPlan {
            queues: &mut self.queues,
            length: 0,
            overflowed: false,
        }
    }
}

/// A plan borrows the actual lock instances until the attempt is settled.
pub struct LockPlan<'object, 'work> {
    queues: &'work mut [Option<&'object Queue>],
    length: usize,
    overflowed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanError {
    Capacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Acquisition {
    /// Queue for the first member and attempt each remaining member once.
    Fast,
    /// Queue for each member in ascending canonical address order.
    Slow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcquisitionError {
    Empty,
    Capacity,
    Context,
    Busy,
}

impl<'object> LockPlan<'object, '_> {
    /// A physical lock that is already present retains its original position.
    pub fn insert<T>(&mut self, lock: &'object TurnLock<T>) -> Result<(), PlanError> {
        if self.queues[..self.length]
            .iter()
            .any(|queue| ptr::eq(member(*queue), &lock.queue))
        {
            return Ok(());
        }
        let Some(slot) = self.queues.get_mut(self.length) else {
            self.overflowed = true;
            return Err(PlanError::Capacity);
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
    pub fn execute<R>(
        self,
        acquisition: Acquisition,
        operation: impl for<'held> FnOnce(&mut HeldLockSet<'held>) -> R,
    ) -> Result<R, AcquisitionError> {
        if self.overflowed {
            return Err(AcquisitionError::Capacity);
        }
        if self.length == 0 {
            return Err(AcquisitionError::Empty);
        }
        let queues = &mut self.queues[..self.length];
        if acquisition == Acquisition::Slow {
            queues.sort_unstable_by_key(|queue| ptr::from_ref(member(*queue)).addr());
        }
        let local = hart::begin_turn(queues.len()).map_err(|error| match error {
            hart::Error::Capacity => AcquisitionError::Capacity,
            hart::Error::Context => AcquisitionError::Context,
        })?;

        // Every node is available and initialized before the first publication.
        for index in 0..queues.len() {
            let node = local.node(index);
            node.next.store(null_mut(), Relaxed);
            node.waiting.store(true, Relaxed);
        }
        let mut held = Held {
            queues,
            local,
            acquired: 0,
        };
        for (index, queue) in queues.iter().enumerate() {
            let queue = member(*queue);
            let node = held.local.node(index);
            if index == 0 || acquisition == Acquisition::Slow {
                queue.acquire(node);
            } else if !queue.attempt(node) {
                return Err(AcquisitionError::Busy);
            }
            held.acquired += 1;
        }
        held.local.held();
        let result = operation(&mut HeldLockSet {
            queues,
            local: PhantomData,
        });
        drop(held);
        Ok(result)
    }
}

fn member(queue: Option<&Queue>) -> &Queue {
    queue.expect("initialized plan member")
}

/// Access to the complete set. Reborrowing this value serializes access even
/// when different roles name the same lock.
pub struct HeldLockSet<'held> {
    queues: &'held [Option<&'held Queue>],
    local: PhantomData<*mut ()>,
}

impl HeldLockSet<'_> {
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
}

struct Held<'object> {
    queues: &'object [Option<&'object Queue>],
    local: TurnParticipation,
    acquired: usize,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.local.releasing();
        for index in (0..self.acquired).rev() {
            member(self.queues[index]).release(self.local.node(index));
        }
        // The local token restores the outer state only after all handoffs.
    }
}

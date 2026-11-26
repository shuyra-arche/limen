use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    mem::DropGuard,
    ptr::{self, null_mut},
    sync::atomic::Ordering::Relaxed,
};

use crate::metal::hart::local::{self, TurnParticipation};

use super::queue::Queue;

/// FIFO protection for ordinary shared state.
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

/// Caller-owned storage for the unique physical locks of one acquisition.
pub struct TurnWorkspace<'object, const N: usize> {
    queues: [Option<&'object Queue>; N],
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

/// A plan borrows the actual lock instances until the attempt is settled.
pub struct TurnPlan<'object, 'work> {
    queues: &'work mut [Option<&'object Queue>],
    length: usize,
    overflowed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnPlanError {
    Capacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnMode {
    /// Queue for the first member and attempt each remaining member once.
    Fast,
    /// Queue for each member in ascending canonical address order.
    Slow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnAcquireError {
    Empty,
    Capacity,
    Context,
    Busy,
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

impl Drop for TurnPlan<'_, '_> {
    fn drop(&mut self) {
        // An abandoned or settled plan leaves no stored member references.
        self.queues[..self.length].fill(None);
    }
}

fn acquire<R>(
    queues: &[Option<&Queue>],
    mode: TurnMode,
    operation: impl FnOnce() -> R,
) -> Result<R, TurnAcquireError> {
    let local = local::begin_turn(queues.len()).map_err(|error| match error {
        local::Error::Capacity => TurnAcquireError::Capacity,
        local::Error::Context => TurnAcquireError::Context,
    })?;
    let mut held = DropGuard::new(
        Held {
            queues,
            local,
            acquired: 0,
        },
        Held::release,
    );

    // Every node is initialized before the first publication.
    for index in 0..queues.len() {
        let node = held.local.node(index);
        node.next.store(null_mut(), Relaxed);
        node.waiting.store(true, Relaxed);
    }
    for (index, queue) in queues.iter().enumerate() {
        let queue = member(*queue);
        let node = held.local.node(index);
        if index == 0 || mode == TurnMode::Slow {
            queue.acquire(node);
        } else if !queue.attempt(node) {
            return Err(TurnAcquireError::Busy);
        }
        held.acquired += 1;
    }
    held.local.held();
    let result = operation();
    drop(held);
    Ok(result)
}

fn member(queue: Option<&Queue>) -> &Queue {
    queue.expect("initialized plan member")
}

/// Access to the complete set. Reborrowing this value serializes access even
/// when different roles name the same lock.
pub struct TurnAccess<'held> {
    queues: &'held [Option<&'held Queue>],
    local: PhantomData<*mut ()>,
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

struct Held<'object> {
    queues: &'object [Option<&'object Queue>],
    local: TurnParticipation,
    acquired: usize,
}

impl Held<'_> {
    fn release(self) {
        self.local.releasing();
        for index in (0..self.acquired).rev() {
            member(self.queues[index]).release(self.local.node(index));
        }
        // The local token restores the outer state only after all handoffs.
    }
}

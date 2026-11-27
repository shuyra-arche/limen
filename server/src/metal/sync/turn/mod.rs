use core::{
    cell::UnsafeCell, marker::PhantomData, mem::DropGuard, ptr::null_mut,
    sync::atomic::Ordering::Relaxed,
};

use super::local::{self, TurnParticipation};

pub(super) mod node;
mod queue;
pub(super) use queue::Queue;

/// FIFO protection for ordinary shared state.
///
/// The lock must have one canonical address for as long as any reference
/// exists. Placing it behind another virtual alias requires external exclusion
/// of all simultaneous access through that alias.
pub struct TurnLock<T> {
    pub(super) queue: Queue,
    pub(super) value: UnsafeCell<T>,
}

// The MCS handoff transfers exclusive access to T to the next owner.
unsafe impl<T: Send> Sync for TurnLock<T> {}

impl<T> TurnLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            queue: Queue::new(),
            value: UnsafeCell::new(value),
        }
    }
}

/// Caller-owned storage for the unique physical locks of one acquisition.
pub struct TurnWorkspace<'object, const N: usize> {
    pub(super) queues: [Option<&'object Queue>; N],
}

/// A plan borrows the actual lock instances until the attempt is settled.
pub struct TurnPlan<'object, 'work> {
    pub(super) queues: &'work mut [Option<&'object Queue>],
    pub(super) length: usize,
    pub(super) overflowed: bool,
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

impl Drop for TurnPlan<'_, '_> {
    fn drop(&mut self) {
        // An abandoned or settled plan leaves no stored member references.
        self.queues[..self.length].fill(None);
    }
}

pub(super) fn acquire<R>(
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

pub(super) fn member(queue: Option<&Queue>) -> &Queue {
    queue.expect("initialized plan member")
}

/// Access to the complete set. Reborrowing this value serializes access even
/// when different roles name the same lock.
pub struct TurnAccess<'held> {
    pub(super) queues: &'held [Option<&'held Queue>],
    pub(super) local: PhantomData<*mut ()>,
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

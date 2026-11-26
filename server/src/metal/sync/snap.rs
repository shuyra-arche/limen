use core::{
    array,
    marker::PhantomData,
    ptr,
    sync::atomic::{
        AtomicU64,
        Ordering::{Acquire, Relaxed, Release},
        fence,
    },
};

use super::leaf::{LeafError, LeafLock};

/// Fixed atomic-word encoding of an independently published pure value.
///
/// # Safety
///
/// Implementations must use a deterministic encoding with finite work and no
/// references, implicit padding, external state, or further lock acquisition.
/// Decoding must safely reconstruct each value produced by the encoder.
pub unsafe trait Snapshot<const WORDS: usize>: Copy + Send {
    fn encode(self) -> [u64; WORDS];
    fn decode(words: [u64; WORDS]) -> Self;
}

/// Failure to observe one complete stable publication in a single attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotBusy;

/// Failure to publish against an observed version of this snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishError {
    Access(LeafError),
    Stale,
    Exhausted,
}

/// Atomic publication of an independent value with a fixed word count.
///
/// The owner sets the word count at construction and safely publishes the
/// whole object before sharing it. A snapshot cannot prove domain authority
/// or the combined state of multiple objects.
pub struct SnapLock<T, const WORDS: usize> {
    writer: LeafLock<()>,
    sequence: AtomicU64,
    words: [AtomicU64; WORDS],
    value: PhantomData<fn() -> T>,
}

/// An observed copy and the particular publication against which it was read.
pub struct Observation<'lock, T, const WORDS: usize> {
    source: &'lock SnapLock<T, WORDS>,
    version: u64,
    value: T,
}

impl<T: Copy, const WORDS: usize> Observation<'_, T, WORDS> {
    pub fn value(&self) -> T {
        self.value
    }
}

impl<T: Snapshot<WORDS>, const WORDS: usize> SnapLock<T, WORDS> {
    pub fn new(value: T) -> Self {
        Self {
            writer: LeafLock::new(()),
            sequence: AtomicU64::new(0),
            words: value.encode().map(AtomicU64::new),
            value: PhantomData,
        }
    }

    /// Reads once with no retry, shared write, or lock participation.
    pub fn observe(&self) -> Result<Observation<'_, T, WORDS>, SnapshotBusy> {
        let first = self.sequence.load(Acquire);
        if first & 1 != 0 {
            return Err(SnapshotBusy);
        }

        let words = array::from_fn(|index| self.words[index].load(Relaxed));
        // If any word came from a newer writer, the writer's release fence
        // synchronizes through that word with this fence. The final sequence
        // read can then observe only the writer's odd value or a later value.
        fence(Acquire);
        if first != self.sequence.load(Relaxed) {
            return Err(SnapshotBusy);
        }

        Ok(Observation {
            source: self,
            version: first,
            value: T::decode(words),
        })
    }

    /// Tries once to publish if the observation is still current.
    pub fn attempt_publish(
        &self,
        expected: &Observation<'_, T, WORDS>,
        value: T,
    ) -> Result<(), PublishError> {
        let words = value.encode();
        self.writer
            .attempt(|()| self.publish_held(expected, words))
            .map_err(PublishError::Access)?
    }

    /// Waits in ordinary context, then publishes if the observation is current.
    pub fn publish(
        &self,
        expected: &Observation<'_, T, WORDS>,
        value: T,
    ) -> Result<(), PublishError> {
        let words = value.encode();
        self.writer
            .acquire(|()| self.publish_held(expected, words))
            .map_err(PublishError::Access)?
    }

    fn publish_held(
        &self,
        expected: &Observation<'_, T, WORDS>,
        words: [u64; WORDS],
    ) -> Result<(), PublishError> {
        let current = self.sequence.load(Relaxed);
        if !ptr::eq(self, expected.source) || current != expected.version {
            return Err(PublishError::Stale);
        }
        let next = current.checked_add(2).ok_or(PublishError::Exhausted)?;

        self.sequence.store(current + 1, Relaxed);
        fence(Release);
        for (word, value) in self.words.iter().zip(words) {
            word.store(value, Relaxed);
        }
        self.sequence.store(next, Release);
        Ok(())
    }
}

use core::{
    marker::PhantomData,
    ptr,
    sync::atomic::{
        AtomicU64,
        Ordering::{Relaxed, Release},
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
    pub(super) writer: LeafLock<()>,
    pub(super) sequence: AtomicU64,
    pub(super) words: [AtomicU64; WORDS],
    value: PhantomData<fn() -> T>,
}

/// An observed copy and the particular publication against which it was read.
pub struct Observation<'lock, T, const WORDS: usize> {
    pub(super) source: &'lock SnapLock<T, WORDS>,
    pub(super) version: u64,
    pub(super) value: T,
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

    pub(super) fn publish_held(
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

use core::{
    array,
    sync::atomic::{
        Ordering::{Acquire, Relaxed},
        fence,
    },
};

use crate::metal::sync::snap::{Observation, PublishError, SnapLock, Snapshot, SnapshotBusy};

impl<T: Copy, const WORDS: usize> Observation<'_, T, WORDS> {
    pub fn value(&self) -> T {
        self.value
    }
}

impl<T: Snapshot<WORDS>, const WORDS: usize> SnapLock<T, WORDS> {
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
}

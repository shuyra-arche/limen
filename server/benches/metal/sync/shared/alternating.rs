//! Two-processor alternating access, including an explicit atomic baseline.
use super::contention::Lock;
use core::{
    hint::spin_loop,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
};

static ATOMIC: AtomicUsize = AtomicUsize::new(0);
static BATON: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
pub(super) enum Work {
    Atomic,
    Locked(Lock),
}

impl Work {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Atomic => "atomic-alternating",
            Self::Locked(Lock::Leaf) => "leaf-alternating",
            Self::Locked(Lock::Turn) => "turn-alternating",
        }
    }

    pub(super) fn reset(self) {
        BATON.store(0, Relaxed);
        match self {
            Self::Atomic => ATOMIC.store(0, Relaxed),
            Self::Locked(lock) => lock.reset(),
        }
    }

    pub(super) fn run(self, slot: usize, iterations: usize) {
        assert!(slot < 2);
        for _ in 0..iterations {
            while BATON.load(Acquire) != slot {
                spin_loop();
            }
            match self {
                Self::Atomic => {
                    ATOMIC.fetch_add(1, Relaxed);
                }
                Self::Locked(lock) => lock.increment(),
            }
            BATON.store(1 - slot, Release);
        }
    }

    pub(super) fn verify(self, iterations: usize) {
        let expected = 2 * iterations;
        match self {
            Self::Atomic => assert_eq!(ATOMIC.load(Acquire), expected),
            Self::Locked(lock) => lock.verify(expected),
        }
        assert_eq!(BATON.load(Acquire), 0);
    }
}

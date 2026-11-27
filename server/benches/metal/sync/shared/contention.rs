//! Shared protected-word increment under unrestricted competition.
use kernel::metal::sync::treaty::{LeafLock, TurnLock};

static LEAF: LeafLock<usize> = LeafLock::new(0);
static TURN: TurnLock<usize> = TurnLock::new(0);

#[derive(Clone, Copy)]
pub(super) enum Lock {
    Leaf,
    Turn,
}

impl Lock {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Leaf => "leaf-shared",
            Self::Turn => "turn-shared",
        }
    }

    pub(super) fn reset(self) {
        match self {
            Self::Leaf => LEAF.acquire(|value| *value = 0).unwrap(),
            Self::Turn => TURN.acquire(|value| *value = 0).unwrap(),
        }
    }

    pub(super) fn increment(self) {
        match self {
            Self::Leaf => LEAF.acquire(|value| *value += 1).unwrap(),
            Self::Turn => TURN.acquire(|value| *value += 1).unwrap(),
        }
    }

    pub(super) fn run(self, iterations: usize) {
        for _ in 0..iterations {
            self.increment();
        }
    }

    pub(super) fn verify(self, expected: usize) {
        let count = match self {
            Self::Leaf => LEAF.acquire(|value| *value).unwrap(),
            Self::Turn => TURN.acquire(|value| *value).unwrap(),
        };
        assert_eq!(count, expected);
    }
}

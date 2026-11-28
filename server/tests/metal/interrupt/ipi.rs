//! One consumer of real coalescible notifications on two processors.

use core::{
    fmt::Debug,
    hint::spin_loop,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
};
use kernel::metal::interrupt::treaty::{Issuance, NotifyProcessor};

pub struct Mailbox {
    work: AtomicUsize,
    issued: AtomicUsize,
    rechecked: AtomicUsize,
}

impl Mailbox {
    pub const fn new() -> Self {
        Self {
            work: AtomicUsize::new(0),
            issued: AtomicUsize::new(0),
            rechecked: AtomicUsize::new(0),
        }
    }

    pub fn publish<S: NotifyProcessor>(&self, sender: &mut S, round: usize, duplicate: bool)
    where
        S::Error: Debug,
        S::Issue: Debug,
    {
        assert_eq!(self.rechecked.load(Acquire), round - 1);
        self.work.store(round, Release);
        let original = Self::issue(sender, None);
        if duplicate {
            Self::issue(sender, Some(original));
            // The controlled duplicate round keeps the local gate masked
            // until both issues. The next round has no sender-to-receiver
            // latch and must recheck after an actual interrupt arrives.
            self.issued.store(round, Release);
        }
    }

    fn issue<S: NotifyProcessor>(sender: &mut S, previous: Option<S::Issue>) -> S::Issue
    where
        S::Error: Debug,
        S::Issue: Debug,
    {
        let mut state = sender.notify().unwrap();
        let original = state.original();
        if let Some(previous) = previous {
            assert_ne!(original, previous);
            assert!(sender.confirm(previous).is_err());
        }
        if matches!(state, Issuance::Pending(_)) {
            assert!(
                sender.notify().is_err(),
                "an original issue remains pending"
            );
            for _ in 0..4096 {
                assert_eq!(sender.pending().unwrap(), Some(original));
                state = sender.confirm(original).unwrap();
                if !matches!(state, Issuance::Pending(_)) {
                    break;
                }
                spin_loop();
            }
        }
        assert!(matches!(state, Issuance::Confirmed(_)), "{state:?}");
        assert_eq!(state.original(), original);
        assert!(sender.pending().unwrap().is_none());
        assert!(sender.confirm(original).is_err());
        original
    }

    #[cfg(target_arch = "riscv64")]
    pub fn ready(&self, round: usize) -> bool {
        self.work.load(Acquire) == round
    }

    pub fn wait_for_issues(&self, round: usize, wait: impl FnOnce(&AtomicUsize, usize)) {
        wait(&self.issued, round);
    }

    /// Called by the actual receiving processor after accepting its native IRQ.
    pub fn recheck(&self, round: usize, finish: impl FnOnce()) {
        assert_eq!(self.work.load(Acquire), round);
        assert_eq!(self.rechecked.load(Relaxed), round - 1);
        finish();
        self.rechecked.store(round, Release);
    }

    pub fn wait_for_recheck(&self, round: usize, wait: impl FnOnce(&AtomicUsize, usize)) {
        wait(&self.rechecked, round);
    }
}

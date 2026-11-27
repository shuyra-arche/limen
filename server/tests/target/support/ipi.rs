//! One consumer of real coalescible notifications on two Harts.

use core::{
    fmt::Debug,
    hint::spin_loop,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
};
use kernel::metal::interrupt::ipi::{Issuance, NotifyHart};

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

    pub fn publish<S: NotifyHart>(&self, sender: &mut S, round: usize, duplicate: bool)
    where
        S::Error: Debug,
    {
        assert_eq!(self.rechecked.load(Acquire), round - 1);
        self.work.store(round, Release);
        Self::issue(sender);
        if duplicate {
            Self::issue(sender);
        }
        // The fixture keeps the receiving gate masked until after both
        // local issues. This latch cannot count as remote work observation.
        self.issued.store(round, Release);
    }

    fn issue<S: NotifyHart>(sender: &mut S)
    where
        S::Error: Debug,
    {
        let mut state = sender.notify().unwrap();
        if matches!(state, Issuance::Pending) {
            assert!(
                sender.notify().is_err(),
                "an original issue remains pending"
            );
            for _ in 0..4096 {
                state = sender.pending().unwrap().expect("retained issue");
                if !matches!(state, Issuance::Pending) {
                    break;
                }
                spin_loop();
            }
        }
        assert!(matches!(state, Issuance::Confirmed), "{state:?}");
        assert!(sender.pending().unwrap().is_none());
    }

    pub fn wait_for_issues(&self, round: usize, wait: impl FnOnce(&AtomicUsize, usize)) {
        wait(&self.issued, round);
    }

    /// Called by the actual receiving Hart after accepting its native IRQ.
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

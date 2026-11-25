use core::arch::asm;
use kernel::sync::{
    Acquisition, AcquisitionError, LeafError, LeafLock, PublishError, SnapLock, Snapshot, TurnLock,
    Work,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pair {
    number: u64,
    complement: u64,
}

impl Pair {
    fn new(number: u64) -> Self {
        Self {
            number,
            complement: !number,
        }
    }
}

// The encoding is exactly two integer fields without padding or other state.
unsafe impl Snapshot<2> for Pair {
    fn encode(self) -> [u64; 2] {
        [self.number, self.complement]
    }

    fn decode([number, complement]: [u64; 2]) -> Self {
        Self { number, complement }
    }
}

pub fn check() {
    let interrupts = local_interrupt_state();
    let leaf = LeafLock::new(0);
    let other = LeafLock::new(1);
    let snap = SnapLock::new(Pair::new(10));
    let prior = snap.read().unwrap();
    assert_eq!(prior.value(), Pair::new(10));
    let number = TurnLock::new(0);
    let flag = TurnLock::new(false);
    let unlisted = TurnLock::new(false);
    let mut work = Work::<2>::new();

    leaf.acquire(|value| {
        assert!(interrupts_masked());
        assert_eq!(leaf.attempt(|_| ()), Err(LeafError::Context));
        assert_eq!(other.attempt(|_| ()), Err(LeafError::Context));
        assert_eq!(other.acquire(|_| ()), Err(LeafError::Context));
        assert_eq!(
            snap.attempt(&prior, Pair::new(11)),
            Err(PublishError::Access(LeafError::Context))
        );
        assert_eq!(snap.read().unwrap().value(), Pair::new(10));
        let mut plan = work.plan();
        plan.insert(&number).unwrap();
        assert_eq!(
            plan.execute(Acquisition::Fast, |_| ()),
            Err(AcquisitionError::Context)
        );
        *value += 1;
    })
    .unwrap();
    assert_eq!(local_interrupt_state(), interrupts);
    assert_eq!(leaf.attempt(|value| *value), Ok(1));

    for acquisition in [Acquisition::Fast, Acquisition::Slow] {
        let mut plan = work.plan();
        plan.insert(&number).unwrap();
        plan.insert(&number).unwrap();
        plan.insert(&flag).unwrap();
        plan.execute(acquisition, |held| {
            assert!(interrupts_masked());
            assert!(held.access(&unlisted).is_none());
            assert_eq!(leaf.acquire(|_| ()), Err(LeafError::Context));
            leaf.attempt(|value| {
                assert!(interrupts_masked());
                *value += 1;
            })
            .unwrap();
            assert!(interrupts_masked());
            let flag = held.access(&flag).unwrap();
            *flag = !*flag;
            *held.access(&number).unwrap() += 1;
        })
        .unwrap();
        assert_eq!(local_interrupt_state(), interrupts);
    }
    assert_eq!(leaf.attempt(|value| *value), Ok(3));
    let mut plan = work.plan();
    plan.insert(&number).unwrap();
    plan.insert(&flag).unwrap();
    plan.execute(Acquisition::Fast, |held| {
        assert_eq!(held.access(&number).map(|value| *value), Some(2));
        assert_eq!(held.access(&flag).map(|value| *value), Some(false));
    })
    .unwrap();

    snap.attempt(&prior, Pair::new(11)).unwrap();
    assert_eq!(
        snap.attempt(&prior, Pair::new(12)),
        Err(PublishError::Stale)
    );
    let current = snap.read().unwrap();
    assert_eq!(current.value(), Pair::new(11));

    let other_snap = SnapLock::new(Pair::new(13));
    let foreign = other_snap.read().unwrap();
    let initial = SnapLock::new(Pair::new(14));
    assert_eq!(
        initial.acquire(&foreign, Pair::new(15)),
        Err(PublishError::Stale)
    );
    snap.acquire(&current, Pair::new(16)).unwrap();
    assert_eq!(snap.read().unwrap().value(), Pair::new(16));
    assert_eq!(local_interrupt_state(), interrupts);
}

#[cfg(target_arch = "riscv64")]
pub fn check_enabled() {
    let enabled_sources: usize;
    unsafe { asm!("csrr {}, sie", out(reg) enabled_sources, options(nostack)) };
    assert_eq!(enabled_sources, 0);
    assert!(interrupts_masked());

    let leaf = LeafLock::new(());
    // The boot fixture proved that all supervisor interrupt sources are
    // disabled, so setting the global bit cannot enter the terminal vector.
    unsafe { asm!("csrsi sstatus, 2", options(nostack)) };
    let masked = leaf.attempt(|()| interrupts_masked());
    let restored = local_interrupt_state();
    unsafe { asm!("csrci sstatus, 2", options(nostack)) };
    assert_eq!(masked, Ok(true));
    assert_eq!(restored, 2);
}

fn local_interrupt_state() -> usize {
    let state: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe {
        asm!("csrr {}, sstatus", out(reg) state, options(nostack));
        return state & 2;
    }
    #[cfg(target_arch = "x86_64")]
    unsafe {
        asm!("pushfq", "pop {}", out(reg) state, options(preserves_flags));
        return state & (1 << 9);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        asm!("mrs {}, daif", out(reg) state, options(nostack));
        return state & (0b1111 << 6);
    }
}

fn interrupts_masked() -> bool {
    #[cfg(any(target_arch = "x86_64", target_arch = "riscv64"))]
    return local_interrupt_state() == 0;
    #[cfg(target_arch = "aarch64")]
    return local_interrupt_state() & (1 << 7) != 0;
}

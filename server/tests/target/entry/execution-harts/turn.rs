use core::{
    hint::spin_loop,
    sync::atomic::{
        AtomicU8,
        Ordering::{Acquire, Release},
    },
};
use kernel::{
    basis::execution::{Current, Error},
    metal::sync::{
        TURN_CAPACITY,
        treaty::{TurnAcquireError, TurnLock, TurnMode, TurnPlanError, TurnWorkspace},
    },
};

static TRANSFER: TurnLock<u64> = TurnLock::new(0);
static COUNT: TurnLock<u64> = TurnLock::new(0);
static PAIR: TurnLock<[u64; 2]> = TurnLock::new([0, !0]);
static LEAVE: TurnLock<()> = TurnLock::new(());
static PHASE: AtomicU8 = AtomicU8::new(0);

const VALUE: u64 = 0x47f2_e36a_591c_8bd0;
const ROUNDS: u64 = 128;

#[repr(u8)]
enum Phase {
    Held = 1,
    Retired,
    Finished,
}

fn publish(phase: Phase) {
    PHASE.store(phase as u8, Release);
}

fn wait(phase: Phase) {
    let phase = phase as u8;
    while PHASE.load(Acquire) < phase {
        spin_loop();
    }
}

pub(super) fn major() {
    check_capacity();

    let mut work = TurnWorkspace::<1>::new();
    let mut plan = work.plan();
    plan.insert(&TRANSFER).unwrap();
    plan.insert(&TRANSFER).expect("two roles share one entry");
    assert_eq!(plan.insert(&COUNT), Err(TurnPlanError::Capacity));
    assert_eq!(
        plan.acquire(TurnMode::Fast, |_| panic!("overflowed plan entered")),
        Err(TurnAcquireError::Capacity)
    );

    let mut plan = work.plan();
    plan.insert(&TRANSFER).unwrap();
    plan.insert(&TRANSFER).unwrap();
    plan.acquire(TurnMode::Fast, |held| {
        assert!(held.access(&COUNT).is_none(), "lock outside the set");
        let mut nested = TurnWorkspace::<1>::new();
        let mut plan = nested.plan();
        plan.insert(&COUNT).unwrap();
        assert_eq!(
            plan.acquire(TurnMode::Fast, |_| panic!("nested Turn entered")),
            Err(TurnAcquireError::Context),
        );

        publish(Phase::Held);
        wait(Phase::Retired);
        // The peer never receives a fixture notification for this write.
        *held.access(&TRANSFER).unwrap() = VALUE;
    })
    .expect("acquire the deduplicated plan");

    increment(false);
    wait(Phase::Finished);

    let mut work = TurnWorkspace::<2>::new();
    let mut plan = work.plan();
    plan.insert(&COUNT).unwrap();
    plan.insert(&PAIR).unwrap();
    plan.acquire(TurnMode::Fast, |held| {
        assert_eq!(*held.access(&COUNT).unwrap(), 2 * ROUNDS);
        assert_eq!(*held.access(&PAIR).unwrap(), [2 * ROUNDS, !(2 * ROUNDS)]);
    })
    .expect("both Harts returned their nodes");
}

fn check_capacity() {
    let locks = [const { TurnLock::new(()) }; TURN_CAPACITY + 1];
    let mut work = TurnWorkspace::<{ TURN_CAPACITY + 1 }>::new();
    let mut plan = work.plan();
    for lock in &locks {
        plan.insert(lock).unwrap();
    }
    assert_eq!(
        plan.acquire(TurnMode::Slow, |_| panic!("oversized plan entered")),
        Err(TurnAcquireError::Capacity),
    );

    let mut plan = work.plan();
    for lock in &locks[..TURN_CAPACITY] {
        plan.insert(lock).unwrap();
    }
    plan.acquire(TurnMode::Slow, |_| ())
        .expect("capacity failure left the admitted node supply available");
}

pub(super) fn minor() {
    wait(Phase::Held);
    let prefix = [const { TurnLock::new(()) }; TURN_CAPACITY - 1];
    let mut work = TurnWorkspace::<TURN_CAPACITY>::new();
    for count in 1..=prefix.len() {
        let mut plan = work.plan();
        for lock in &prefix[..count] {
            plan.insert(lock).unwrap();
        }
        plan.insert(&TRANSFER).unwrap();
        let retry = Retry(&prefix[0]);
        assert_eq!(
            plan.acquire(TurnMode::Fast, move |_| {
                drop(retry);
                panic!("incomplete set entered");
            }),
            Err(TurnAcquireError::Busy),
        );
    }

    let mut plan = work.plan();
    for lock in &prefix {
        plan.insert(lock).unwrap();
    }
    plan.acquire(TurnMode::Slow, |_| ())
        .expect("Busy released each acquired prefix");
    publish(Phase::Retired);

    let value = TRANSFER
        .acquire(|value| *value)
        .expect("handoff of the contended lock");
    assert_eq!(value, VALUE);

    increment(true);
    publish(Phase::Finished);
}

struct Retry<'a>(&'a TurnLock<()>);

impl Drop for Retry<'_> {
    fn drop(&mut self) {
        self.0
            .acquire(|_| ())
            .expect("callback capture is dropped after the Busy prefix is released");
    }
}

pub(super) fn check_leave(current: &mut Current) {
    let result = LEAVE
        .acquire(|_| current.yield_now())
        .expect("occupied Unit can acquire an ordinary Turn");
    assert_eq!(result, Err(Error::Protection));
}

fn increment(reverse: bool) {
    let mut work = TurnWorkspace::<2>::new();
    for _ in 0..ROUNDS {
        let mut plan = work.plan();
        if reverse {
            plan.insert(&PAIR).unwrap();
            plan.insert(&COUNT).unwrap();
        } else {
            plan.insert(&COUNT).unwrap();
            plan.insert(&PAIR).unwrap();
        }
        plan.acquire(TurnMode::Slow, |held| {
            let count = *held.access(&COUNT).unwrap();
            let pair = held.access(&PAIR).unwrap();
            assert_eq!(*pair, [count, !count]);
            let next = count + 1;
            *pair = [next, !next];
            *held.access(&COUNT).unwrap() = next;
        })
        .expect("one stable order for both insertion orders");
    }
}

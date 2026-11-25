use core::{
    hint::spin_loop,
    sync::atomic::{
        AtomicU8,
        Ordering::{Acquire, Release},
    },
};
use kernel::{
    entry::execution::{Current, Error},
    sync::{Acquisition, AcquisitionError, PlanError, TurnLock, Work},
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

    let mut work = Work::<1>::new();
    let mut plan = work.plan();
    plan.insert(&TRANSFER).unwrap();
    plan.insert(&TRANSFER).expect("two roles share one entry");
    assert_eq!(plan.insert(&COUNT), Err(PlanError::Capacity));

    let mut plan = work.plan();
    plan.insert(&TRANSFER).unwrap();
    plan.insert(&TRANSFER).unwrap();
    plan.execute(Acquisition::Fast, |held| {
        assert!(held.access(&COUNT).is_none(), "lock outside the set");
        let mut nested = Work::<1>::new();
        let mut plan = nested.plan();
        plan.insert(&COUNT).unwrap();
        assert_eq!(
            plan.execute(Acquisition::Fast, |_| panic!("nested Turn entered")),
            Err(AcquisitionError::Context),
        );

        publish(Phase::Held);
        wait(Phase::Retired);
        // The peer never receives a fixture notification for this write.
        *held.access(&TRANSFER).unwrap() = VALUE;
    })
    .expect("acquire the deduplicated plan");

    increment(false);
    wait(Phase::Finished);

    let mut work = Work::<2>::new();
    let mut plan = work.plan();
    plan.insert(&COUNT).unwrap();
    plan.insert(&PAIR).unwrap();
    plan.execute(Acquisition::Fast, |held| {
        assert_eq!(*held.access(&COUNT).unwrap(), 2 * ROUNDS);
        assert_eq!(*held.access(&PAIR).unwrap(), [2 * ROUNDS, !(2 * ROUNDS)]);
    })
    .expect("both Harts returned their nodes");
}

fn check_capacity() {
    // This build provisions four nodes per Hart for admitted Turn plans.
    let locks = [const { TurnLock::new(()) }; 5];
    let mut work = Work::<5>::new();
    let mut plan = work.plan();
    for lock in &locks {
        plan.insert(lock).unwrap();
    }
    assert_eq!(
        plan.execute(Acquisition::Slow, |_| panic!("oversized plan entered")),
        Err(AcquisitionError::Capacity),
    );

    let mut plan = work.plan();
    for lock in &locks[..4] {
        plan.insert(lock).unwrap();
    }
    plan.execute(Acquisition::Slow, |_| ())
        .expect("capacity failure left all four nodes available");
}

pub(super) fn minor() {
    wait(Phase::Held);
    let mut work = Work::<3>::new();
    let mut plan = work.plan();
    plan.insert(&COUNT).unwrap();
    plan.insert(&PAIR).unwrap();
    plan.insert(&TRANSFER).unwrap();
    assert_eq!(
        plan.execute(Acquisition::Fast, |_| panic!("incomplete set entered")),
        Err(AcquisitionError::Busy),
    );

    let mut plan = work.plan();
    plan.insert(&COUNT).unwrap();
    plan.insert(&PAIR).unwrap();
    plan.execute(Acquisition::Fast, |held| {
        assert_eq!(*held.access(&COUNT).unwrap(), 0);
        assert_eq!(*held.access(&PAIR).unwrap(), [0, !0]);
    })
    .expect("Busy released both acquired prefixes");
    publish(Phase::Retired);

    let mut plan = work.plan();
    plan.insert(&TRANSFER).unwrap();
    let value = plan
        .execute(Acquisition::Fast, |held| *held.access(&TRANSFER).unwrap())
        .expect("handoff of the contended lock");
    assert_eq!(value, VALUE);

    increment(true);
    publish(Phase::Finished);
}

pub(super) fn check_leave(current: &mut Current) {
    let mut work = Work::<1>::new();
    let mut plan = work.plan();
    plan.insert(&LEAVE).unwrap();
    let result = plan
        .execute(Acquisition::Fast, |_| current.yield_now())
        .expect("occupied Unit can acquire an ordinary Turn");
    assert_eq!(result, Err(Error::State));
}

fn increment(reverse: bool) {
    let mut work = Work::<2>::new();
    for _ in 0..ROUNDS {
        let mut plan = work.plan();
        if reverse {
            plan.insert(&PAIR).unwrap();
            plan.insert(&COUNT).unwrap();
        } else {
            plan.insert(&COUNT).unwrap();
            plan.insert(&PAIR).unwrap();
        }
        plan.execute(Acquisition::Slow, |held| {
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

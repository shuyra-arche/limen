include!("../../../../src/basis/sync.rs");

use core::sync::atomic::AtomicU8;

static LOCK: Lock<u8> = Lock::new(0);
static PHASE: AtomicU8 = AtomicU8::new(0);

fn wait_phase(phase: u8) {
    while PHASE.load(Acquire) < phase {
        core::hint::spin_loop();
    }
}

pub(super) fn owner() {
    let guards = Cell::new(0);
    {
        let mut guard = LOCK.lock(&guards);
        *guard = 1;
        PHASE.store(1, Release);
        wait_phase(2);
    }
    // The waiter owns the next ticket but is still stopped before acquiring.
    assert!(
        LOCK.try_lock(&guards).is_none(),
        "try overtook a queued ticket"
    );
    PHASE.store(3, Release);
    wait_phase(4);
    assert_eq!(*LOCK.try_lock(&guards).expect("queue is empty"), 2);
    assert_eq!(guards.get(), 0);
}

pub(super) fn waiter() {
    let guards = Cell::new(0);
    wait_phase(1);
    let ticket = LOCK.enqueue(&guards);
    PHASE.store(2, Release);
    wait_phase(3);
    {
        let mut guard = LOCK.wait(ticket, &guards);
        assert_eq!(*guard, 1);
        *guard = 2;
    }
    PHASE.store(4, Release);
}

use core::{
    hint::spin_loop,
    ptr,
    sync::atomic::{
        AtomicPtr, AtomicU64,
        Ordering::{Acquire, Release},
    },
};
use kernel::metal::sync::treaty::{SnapLock, Snapshot};

const LAST: u64 = 4096;
const BATCH: u64 = 128;

#[derive(Clone, Copy)]
struct Pair([u64; 2]);

// The test value has a fixed representation of two independent u64 words.
unsafe impl Snapshot<2> for Pair {
    fn encode(self) -> [u64; 2] {
        self.0
    }

    fn decode(words: [u64; 2]) -> Self {
        Self(words)
    }
}

static SHARED: AtomicPtr<SnapLock<Pair, 2>> = AtomicPtr::new(ptr::null_mut());
static OBSERVED: AtomicU64 = AtomicU64::new(0);

pub fn publish() {
    let snap = SnapLock::new(Pair([0, !0]));
    SHARED.store(ptr::from_ref(&snap).cast_mut(), Release);

    for number in 1..=LAST {
        let expected = snap.observe().unwrap();
        snap.publish(&expected, Pair([number, !number])).unwrap();
        if number.is_multiple_of(BATCH) {
            while OBSERVED.load(Acquire) < number {
                spin_loop();
            }
        }
    }

    // The last observation is acknowledged only after the minor ends its
    // access. Until that acknowledgement this stack slot remains in use.
    SHARED.store(ptr::null_mut(), Release);
}

pub fn observe() {
    let source = loop {
        let source = SHARED.load(Acquire);
        if !source.is_null() {
            break source;
        }
        spin_loop();
    };
    // The major keeps this source alive until the acknowledgement below.
    let snap = unsafe { &*source };
    let mut previous = 0;
    loop {
        let Ok(sample) = snap.observe() else {
            spin_loop();
            continue;
        };
        let Pair([number, complement]) = sample.value();
        assert_eq!(complement, !number);
        assert!((previous..=LAST).contains(&number));
        if number == LAST {
            break;
        }
        if number != previous {
            OBSERVED.store(number, Release);
            previous = number;
        }
    }
    OBSERVED.store(LAST, Release);
}

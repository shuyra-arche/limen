use core::{
    hint::spin_loop,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
};
use guest::{Boot, bench};
use kernel::metal::{
    chrono::{
        riscv64::{description::RiscvTimebase, time::Counter},
        treaty::Clock,
    },
    processor::treaty::{LocalStorage, Minor, Stack, Startup},
    sync::treaty::{LeafLock, Supply, TurnLock},
};

const PEERS: usize = 3;
const HARTS: usize = PEERS + 1;
const WARMUP: usize = 512;
const ITERATIONS: usize = 4096;
const SAMPLES: usize = 9;

static STACKS: [Stack<{ 32 * 1024 }>; PEERS] = [const { Stack::new() }; PEERS];
static LOCALS: [LocalStorage<5>; PEERS] = [const { LocalStorage::new(Supply::new()) }; PEERS];
static MINORS: [Minor; PEERS] = [
    Minor::new(&STACKS[0], &LOCALS[0]),
    Minor::new(&STACKS[1], &LOCALS[1]),
    Minor::new(&STACKS[2], &LOCALS[2]),
];

static LEAF: LeafLock<usize> = LeafLock::new(0);
static TURN: TurnLock<usize> = TurnLock::new(0);
static ATOMIC: AtomicUsize = AtomicUsize::new(0);
static BATON: AtomicUsize = AtomicUsize::new(0);
static JOINED: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
#[repr(usize)]
enum Mode {
    LeafShared = 1,
    TurnShared,
    AtomicAlternating,
    LeafAlternating,
    TurnAlternating,
}

const MODES: [Mode; 5] = [
    Mode::LeafShared,
    Mode::TurnShared,
    Mode::AtomicAlternating,
    Mode::LeafAlternating,
    Mode::TurnAlternating,
];

impl Mode {
    fn decode(value: usize) -> Self {
        MODES[value.checked_sub(1).unwrap()]
    }

    fn name(self) -> &'static str {
        match self {
            Self::LeafShared => "leaf-shared",
            Self::TurnShared => "turn-shared",
            Self::AtomicAlternating => "atomic-alternating",
            Self::LeafAlternating => "leaf-alternating",
            Self::TurnAlternating => "turn-alternating",
        }
    }

    fn alternating(self) -> bool {
        matches!(
            self,
            Self::AtomicAlternating | Self::LeafAlternating | Self::TurnAlternating
        )
    }

    fn participants(self) -> usize {
        if self.alternating() { 2 } else { HARTS }
    }

    fn reset(self) {
        BATON.store(0, Relaxed);
        match self {
            Self::LeafShared | Self::LeafAlternating => {
                LEAF.acquire(|value| *value = 0).unwrap();
            }
            Self::TurnShared | Self::TurnAlternating => {
                TURN.acquire(|value| *value = 0).unwrap();
            }
            Self::AtomicAlternating => ATOMIC.store(0, Relaxed),
        }
    }

    fn run(self, slot: usize, iterations: usize) {
        let alternating = self.alternating();
        match self {
            Self::LeafShared | Self::LeafAlternating => {
                repeat(slot, iterations, alternating, || {
                    LEAF.acquire(|value| *value += 1).unwrap();
                });
            }
            Self::TurnShared | Self::TurnAlternating => {
                repeat(slot, iterations, alternating, || {
                    TURN.acquire(|value| *value += 1).unwrap();
                });
            }
            Self::AtomicAlternating => repeat(slot, iterations, true, || {
                ATOMIC.fetch_add(1, Relaxed);
            }),
        }
    }

    fn verify(self, iterations: usize) {
        let count = match self {
            Self::LeafShared | Self::LeafAlternating => LEAF.acquire(|value| *value).unwrap(),
            Self::TurnShared | Self::TurnAlternating => TURN.acquire(|value| *value).unwrap(),
            Self::AtomicAlternating => ATOMIC.load(Acquire),
        };
        assert_eq!(count, iterations * self.participants());
        if self.alternating() {
            assert_eq!(BATON.load(Acquire), 0);
        }
    }
}

fn repeat(slot: usize, iterations: usize, alternating: bool, mut work: impl FnMut()) {
    if alternating {
        assert!(slot < 2);
        for _ in 0..iterations {
            while BATON.load(Acquire) != slot {
                spin_loop();
            }
            work();
            BATON.store(1 - slot, Release);
        }
    } else {
        for _ in 0..iterations {
            work();
        }
    }
}

struct Batch {
    epoch: AtomicUsize,
    gate: AtomicUsize,
    mode: AtomicUsize,
    iterations: AtomicUsize,
    ready: [AtomicUsize; PEERS],
    done: [AtomicUsize; PEERS],
}

static BATCH: Batch = Batch {
    epoch: AtomicUsize::new(0),
    gate: AtomicUsize::new(0),
    mode: AtomicUsize::new(0),
    iterations: AtomicUsize::new(0),
    ready: [const { AtomicUsize::new(0) }; PEERS],
    done: [const { AtomicUsize::new(0) }; PEERS],
};

fn wait(signal: &AtomicUsize, expected: usize) {
    while signal.load(Acquire) != expected {
        spin_loop();
    }
}

impl Batch {
    fn minor(&self, slot: usize) -> ! {
        let mut previous = 0;
        loop {
            let epoch = previous + 1;
            wait(&self.epoch, epoch);
            let mode = Mode::decode(self.mode.load(Relaxed));
            let iterations = self.iterations.load(Relaxed);
            if slot >= mode.participants() {
                // Inactive Harts acknowledge before timing begins.
                self.done[slot - 1].store(epoch, Release);
                self.ready[slot - 1].store(epoch, Release);
            } else {
                self.ready[slot - 1].store(epoch, Release);
                wait(&self.gate, epoch);
                mode.run(slot, iterations);
                // No lock acquisition or protected value survives this receipt.
                self.done[slot - 1].store(epoch, Release);
            }
            previous = epoch;
        }
    }

    fn run(&self, clock: &Clock, mode: Mode, iterations: usize, timed: bool) -> u64 {
        // Every previous participant is settled before publishing a new batch.
        mode.reset();
        let epoch = self.epoch.load(Relaxed) + 1;
        self.mode.store(mode as usize, Relaxed);
        self.iterations.store(iterations, Relaxed);
        self.epoch.store(epoch, Release);
        for ready in &self.ready {
            wait(ready, epoch);
        }

        let work = || {
            self.gate.store(epoch, Release);
            mode.run(0, iterations);
            for done in &self.done {
                wait(done, epoch);
            }
        };
        let elapsed = if timed {
            bench::measure(clock, work)
        } else {
            work();
            0
        };
        mode.verify(iterations);
        elapsed
    }
}

fn minor(hart: usize, slot: usize) -> ! {
    assert!((1..HARTS).contains(&slot));
    assert!(hart < HARTS);
    assert!(MINORS[slot - 1].ready());
    let bit = 1 << hart;
    assert_eq!(JOINED.fetch_or(bit, Release) & bit, 0);
    BATCH.minor(slot)
}

pub fn check(boot: Boot<'_>) {
    let major = boot.hart();
    assert!(major < HARTS);
    for hart in 0..HARTS {
        assert!(boot.riscv_cpus().cpu(hart as u64).unwrap().unwrap().enabled());
    }
    assert!(boot.riscv_cpus().cpu(HARTS as u64).unwrap().is_none());
    let timebase = RiscvTimebase::describe(boot.riscv_cpus()).unwrap().unwrap();
    let counter = unsafe { Counter::admit(timebase, major as u64) }.unwrap();
    let clock = counter.clock();

    JOINED.store(1 << major, Release);
    let hsm = boot.sbi_services().hart_state_management().unwrap().unwrap();
    let mut startup = Startup::Sbi(hsm);
    for (index, hart) in (0..HARTS).filter(|&hart| hart != major).enumerate() {
        unsafe { MINORS[index].start(hart, minor, index + 1, &mut startup) }.unwrap();
    }
    wait(&JOINED, (1 << HARTS) - 1);

    let mut samples = [[0_u64; SAMPLES]; MODES.len()];
    for (mode, elapsed) in MODES.into_iter().zip(&mut samples) {
        BATCH.run(clock, mode, WARMUP, false);
        for sample in elapsed {
            *sample = BATCH.run(clock, mode, ITERATIONS, true);
        }
    }
    let series: [bench::Series<'_>; MODES.len()] = core::array::from_fn(|index| {
        let mode = MODES[index];
        let participants = mode.participants();
        bench::Series {
            name: mode.name(),
            participants,
            warmup_iterations: WARMUP * participants,
            iterations: ITERATIONS * participants,
            elapsed_ns: &samples[index],
        }
    });
    bench::report(clock, &series);
}

mod metal {
    pub mod hart {
        include!("../../../src/metal/hart/node.rs");
    }
}

mod queue {
    // Observe the first spin on each test thread without instrumenting the
    // kernel. The queue still executes the production atomic operations.
    mod core {
        pub use ::core::{ptr, sync};

        pub mod hint {
            pub use super::super::observe_spin as spin_loop;
        }
    }

    mod spin {
        use std::{
            cell::Cell,
            sync::atomic::{AtomicBool, Ordering::Release},
        };

        thread_local! {
            static SIGNAL: Cell<Option<&'static AtomicBool>> = const { Cell::new(None) };
        }

        pub(super) fn arm(signal: &'static AtomicBool) {
            SIGNAL.set(Some(signal));
        }

        pub fn observe() {
            if let Some(signal) = SIGNAL.take() {
                signal.store(true, Release);
            }
            ::core::hint::spin_loop();
        }
    }

    use spin::arm;
    pub use spin::observe as observe_spin;

    include!("../../../src/basis/sync/queue.rs");

    use std::{
        sync::atomic::{AtomicBool, AtomicU8},
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    fn until(what: &str, ready: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::yield_now();
        }
    }

    fn join(thread: JoinHandle<()>) {
        until("queue participant to finish", || thread.is_finished());
        thread.join().unwrap();
    }

    fn prepare(node: &QueueNode) {
        node.next.store(null_mut(), Relaxed);
        node.waiting.store(true, Relaxed);
    }

    #[test]
    fn release_waits_for_an_exchanged_successor_to_link() {
        static QUEUE: Queue = Queue::new();
        static OWNER: QueueNode = QueueNode::new();
        static SUCCESSOR: QueueNode = QueueNode::new();
        static CONTENDER: QueueNode = QueueNode::new();
        static RELEASING: AtomicBool = AtomicBool::new(false);

        prepare(&OWNER);
        prepare(&SUCCESSOR);
        prepare(&CONTENDER);
        assert!(QUEUE.attempt(&OWNER));

        // Publish exactly the first atomic effect of a successor and defer
        // publishing its link until the real release reaches the wait.
        let successor = ptr::from_ref(&SUCCESSOR).cast_mut();
        assert_eq!(
            QUEUE.tail.swap(successor, AcqRel),
            ptr::from_ref(&OWNER).cast_mut(),
        );
        let owner = thread::spawn(|| {
            arm(&RELEASING);
            QUEUE.release(&OWNER);
        });
        until("owner to wait for the link", || RELEASING.load(Acquire));
        assert!(!owner.is_finished());
        assert!(SUCCESSOR.waiting.load(Acquire));
        assert!(!QUEUE.attempt(&CONTENDER), "attempt must not bypass a link");

        OWNER.next.store(successor, Release);
        join(owner);
        assert!(!SUCCESSOR.waiting.load(Acquire));
        assert!(!QUEUE.attempt(&CONTENDER), "successor holds the queue");
        join(thread::spawn(|| QUEUE.release(&SUCCESSOR)));

        prepare(&OWNER);
        assert!(QUEUE.attempt(&OWNER), "the old node can be reused");
        join(thread::spawn(|| QUEUE.release(&OWNER)));
        assert!(QUEUE.tail.load(Acquire).is_null());
    }

    #[test]
    fn two_linked_successors_take_the_baton_in_fifo_order() {
        static QUEUE: Queue = Queue::new();
        static OWNER: QueueNode = QueueNode::new();
        static FIRST: QueueNode = QueueNode::new();
        static SECOND: QueueNode = QueueNode::new();
        static CONTENDER: QueueNode = QueueNode::new();
        static FIRST_WAITING: AtomicBool = AtomicBool::new(false);
        static SECOND_WAITING: AtomicBool = AtomicBool::new(false);
        static ORDER: AtomicU8 = AtomicU8::new(0);

        for node in [&OWNER, &FIRST, &SECOND, &CONTENDER] {
            prepare(node);
        }
        assert!(QUEUE.attempt(&OWNER));
        let first = thread::spawn(|| {
            arm(&FIRST_WAITING);
            QUEUE.acquire(&FIRST);
            assert_eq!(ORDER.fetch_add(1, Relaxed), 0);
            QUEUE.release(&FIRST);
        });
        until("first successor to wait", || FIRST_WAITING.load(Acquire));
        let second = thread::spawn(|| {
            arm(&SECOND_WAITING);
            QUEUE.acquire(&SECOND);
            assert_eq!(ORDER.fetch_add(1, Relaxed), 1);
            QUEUE.release(&SECOND);
        });
        until("second successor to wait", || SECOND_WAITING.load(Acquire));

        assert!(!QUEUE.attempt(&CONTENDER));
        join(thread::spawn(|| QUEUE.release(&OWNER)));
        join(first);
        join(second);
        assert_eq!(ORDER.load(Relaxed), 2);
        assert!(QUEUE.tail.load(Acquire).is_null());
    }
}

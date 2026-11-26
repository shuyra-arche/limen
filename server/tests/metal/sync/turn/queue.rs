use super::{AcqRel, Acquire, Departure, Queue, QueueNode, Relaxed, Release, null_mut, ptr};

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
fn release_requires_the_link_from_an_exchanged_successor() {
    static QUEUE: Queue = Queue::new();
    static OWNER: QueueNode = QueueNode::new();
    static SUCCESSOR: QueueNode = QueueNode::new();
    static CONTENDER: QueueNode = QueueNode::new();

    static DECIDED: AtomicBool = AtomicBool::new(false);

    prepare(&OWNER);
    prepare(&SUCCESSOR);
    prepare(&CONTENDER);
    assert!(QUEUE.attempt(&OWNER));

    // Publish the first atomic effect of a successor, but not its link.
    // Execute the real release decision once at this exact boundary.
    let successor = ptr::from_ref(&SUCCESSOR).cast_mut();
    assert_eq!(
        QUEUE.tail.swap(successor, AcqRel),
        ptr::from_ref(&OWNER).cast_mut(),
    );
    let owner = thread::spawn(|| {
        let departure = QUEUE.departure(&OWNER);
        assert!(matches!(&departure, Departure::AwaitLink(_)));
        // Observe a completed real decision, not an injected spin hook.
        DECIDED.store(true, Release);
        departure.finish();
    });
    until("release decision", || DECIDED.load(Acquire));
    assert!(OWNER.next.load(Acquire).is_null());
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
    static ORDER: AtomicU8 = AtomicU8::new(0);

    for node in [&OWNER, &FIRST, &SECOND, &CONTENDER] {
        prepare(node);
    }
    assert!(QUEUE.attempt(&OWNER));
    let first = thread::spawn(|| {
        QUEUE.acquire(&FIRST);
        assert_eq!(ORDER.fetch_add(1, Relaxed), 0);
        QUEUE.release(&FIRST);
    });
    until("first successor to link", || {
        OWNER.next.load(Acquire) == ptr::from_ref(&FIRST).cast_mut()
    });
    let second = thread::spawn(|| {
        QUEUE.acquire(&SECOND);
        assert_eq!(ORDER.fetch_add(1, Relaxed), 1);
        QUEUE.release(&SECOND);
    });
    until("second successor to link", || {
        FIRST.next.load(Acquire) == ptr::from_ref(&SECOND).cast_mut()
    });

    assert!(!QUEUE.attempt(&CONTENDER));
    join(thread::spawn(|| QUEUE.release(&OWNER)));
    join(first);
    join(second);
    assert_eq!(ORDER.load(Relaxed), 2);
    assert!(QUEUE.tail.load(Acquire).is_null());
}

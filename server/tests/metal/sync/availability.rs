use kernel::metal::sync::{LeafError, LeafLock, TurnAcquireError, TurnLock, turn_capacity};

#[test]
fn ordinary_host_cannot_acquire_hart_protection() {
    assert_eq!(turn_capacity(), None);
    assert_eq!(
        LeafLock::new(()).attempt(|_| panic!("no installed Hart")),
        Err(LeafError::Context)
    );
    assert_eq!(
        TurnLock::new(()).acquire(|_| panic!("no installed Hart")),
        Err(TurnAcquireError::Context)
    );
}

use kernel::metal::sync::treaty::{LeafError, LeafLock};

#[test_case]
fn leaf_rejects_reentrant_access_and_releases() {
    let lock = LeafLock::new(0_u64);
    assert_eq!(
        lock.acquire(|value| {
            assert_eq!(lock.attempt(|_| ()), Err(LeafError::Context));
            *value += 1;
            *value
        }),
        Ok(1)
    );
    assert_eq!(lock.attempt(|value| *value), Ok(1));
}

// T1 deliberately rejects one automatically collected case in a separate
// infrastructure test. No test list or runner change identifies this function.
#[test_case]
fn collection_reaches_nested_modules() {
    if option_env!("LIMEN_T0_CASE") == Some("discovery-panic") {
        panic!("T1 intentional collected test failure");
    }
}

mod leaf;
#[path = "../../../../metal/sync/native.rs"]
mod native;
use kernel::metal::sync::treaty::TurnLock;

#[test_case]
fn turn_releases_the_original_value() {
    let lock = TurnLock::new(0_u64);
    assert_eq!(
        lock.acquire(|value| {
            *value += 1;
            *value
        }),
        Ok(1)
    );
    assert_eq!(lock.acquire(|value| *value), Ok(1));
}

#[test_case]
fn original_synchronization_contract() {
    native::check();
}

#[cfg(target_arch = "riscv64")]
#[test_case]
fn original_enabled_interrupt_contract() {
    native::check_enabled();
}

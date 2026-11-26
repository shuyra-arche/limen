#![no_std]

use kernel::metal::sync::{TurnMode, TurnAcquireError, TurnLock, TurnWorkspace};

pub fn transfer(count: &TurnLock<u32>, open: &TurnLock<bool>) -> Result<bool, TurnAcquireError> {
    let mut work = TurnWorkspace::<2>::new();
    let mut plan = work.plan();
    plan.insert(count).unwrap();
    plan.insert(open).unwrap();
    plan.acquire(TurnMode::Slow, |held| {
        *held.access(count).unwrap() += 1;
        *held.access(open).unwrap()
    })
}

pub fn increment(count: &TurnLock<u32>) -> Result<(), TurnAcquireError> {
    count.acquire(|count| *count += 1)
}

#[cfg(contract = "escape")]
pub fn escape(lock: &TurnLock<u32>) {
    let mut work = TurnWorkspace::<1>::new();
    let mut plan = work.plan();
    plan.insert(lock).unwrap();
    let mut escaped = None;
    plan.acquire(TurnMode::Fast, |held| escaped = held.access(lock))
        .unwrap();
    let _ = escaped.unwrap();
}

#[cfg(contract = "escape-single")]
pub fn escape_single(lock: &TurnLock<u32>) {
    let mut escaped = None;
    lock.acquire(|value| escaped = Some(value)).unwrap();
    let _ = escaped.unwrap();
}

#[cfg(contract = "alias")]
pub fn alias(lock: &TurnLock<u32>) {
    let mut work = TurnWorkspace::<1>::new();
    let mut plan = work.plan();
    plan.insert(lock).unwrap();
    plan.acquire(TurnMode::Fast, |held| {
        let first = held.access(lock).unwrap();
        let second = held.access(lock).unwrap();
        *first += *second;
    })
    .unwrap();
}

#[cfg(contract = "forget")]
pub fn forget(lock: &TurnLock<u32>) {
    let mut work = TurnWorkspace::<1>::new();
    let mut plan = work.plan();
    plan.insert(lock).unwrap();
    plan.acquire(TurnMode::Fast, |held| core::mem::forget(*held))
        .unwrap();
}

#[cfg(contract = "send")]
pub fn send() {
    fn require_send<T: Send>() {}
    require_send::<kernel::metal::sync::TurnAccess<'static>>();
}

#[cfg(contract = "sync")]
pub fn sync() {
    fn require_sync<T: Sync>() {}
    require_sync::<kernel::metal::sync::TurnAccess<'static>>();
}

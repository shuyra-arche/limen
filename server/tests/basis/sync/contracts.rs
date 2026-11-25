#![no_std]

use kernel::sync::{Acquisition, AcquisitionError, TurnLock, Work};

pub fn transfer(count: &TurnLock<u32>, open: &TurnLock<bool>) -> Result<bool, AcquisitionError> {
    let mut work = Work::<2>::new();
    let mut plan = work.plan();
    plan.insert(count).unwrap();
    plan.insert(open).unwrap();
    plan.execute(Acquisition::Slow, |held| {
        *held.access(count).unwrap() += 1;
        *held.access(open).unwrap()
    })
}

#[cfg(contract = "escape")]
pub fn escape(lock: &TurnLock<u32>) {
    let mut work = Work::<1>::new();
    let mut plan = work.plan();
    plan.insert(lock).unwrap();
    let mut escaped = None;
    plan.execute(Acquisition::Fast, |held| escaped = held.access(lock))
        .unwrap();
    let _ = escaped.unwrap();
}

#[cfg(contract = "alias")]
pub fn alias(lock: &TurnLock<u32>) {
    let mut work = Work::<1>::new();
    let mut plan = work.plan();
    plan.insert(lock).unwrap();
    plan.execute(Acquisition::Fast, |held| {
        let first = held.access(lock).unwrap();
        let second = held.access(lock).unwrap();
        *first += *second;
    })
    .unwrap();
}

#[cfg(contract = "send")]
pub fn send() {
    fn require_send<T: Send>() {}
    require_send::<kernel::sync::HeldLockSet<'static>>();
}

#[cfg(contract = "sync")]
pub fn sync() {
    fn require_sync<T: Sync>() {}
    require_sync::<kernel::sync::HeldLockSet<'static>>();
}

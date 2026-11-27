//! Native AMD-Vi command ring, exact completion, and progress.

use super::{CONTROL, Controller, ENABLE, Error, RUNNING, STATUS, State, TAIL, directory::DENY};
use core::{
    cell::UnsafeCell,
    sync::atomic::{Ordering, fence},
};

#[repr(C, align(4096))]
pub struct Queue([[u64; 2]; 256]);

impl Queue {
    pub const EMPTY: Self = Self([[0; 2]; 256]);
}

#[repr(C, align(8))]
pub struct Completion(pub(super) UnsafeCell<u64>);

impl Completion {
    pub const fn zeroed() -> Self {
        Self(UnsafeCell::new(0))
    }
}

#[derive(Clone, Copy)]
pub(super) enum Operation {
    InstallRequester(u16),
    InstallAll,
    Bind(usize),
    Unbind(usize),
}

#[derive(Clone, Copy)]
pub(super) struct Batch {
    pub(super) number: u64,
    pub(super) operation: Operation,
}

pub(super) enum Progress {
    Fresh,
    Starting,
    Waiting(Batch),
    Ready,
    Unconfirmed(Option<Batch>),
}

impl Controller<'_> {
    pub fn state(&self) -> State {
        match self.progress {
            Progress::Fresh => State::Fresh,
            Progress::Starting => State::Starting,
            Progress::Waiting(batch) => State::Waiting(batch.number),
            Progress::Ready => State::Ready,
            Progress::Unconfirmed(_) => State::Unconfirmed,
        }
    }

    /// When completion is uncertain, keep the exact operation and both
    /// potentially observed translations. In particular, an unbind leaves
    /// its original binding reserved until an independent retirement.
    pub fn unsettled_generation(&self) -> Option<u64> {
        match self.progress {
            Progress::Waiting(batch) | Progress::Unconfirmed(Some(batch)) => Some(batch.number),
            _ => None,
        }
    }

    pub(super) fn uncertain(&mut self) -> Error {
        let pending = match self.progress {
            Progress::Waiting(batch) => Some(batch),
            _ => None,
        };
        self.progress = Progress::Unconfirmed(pending);
        Error::Unconfirmed
    }

    fn next_install_batch(&mut self) -> Result<(), Error> {
        let operation = self
            .selections
            .next()
            .map_or(Operation::InstallAll, Operation::InstallRequester);
        let number = self.next.checked_add(1).ok_or_else(|| self.uncertain())?;
        self.next = number;
        self.invalidate(number, operation);
        Ok(())
    }

    pub(super) fn invalidate(&mut self, number: u64, operation: Operation) {
        self.progress = Progress::Waiting(Batch { number, operation });
        // At most three slots are used, and the previous completion store
        // has already settled. No independent producer advances this tail.
        let commands = &mut self.storage.queue.0.0;
        let requester = match operation {
            Operation::InstallRequester(requester) => Some(requester),
            Operation::Bind(index) | Operation::Unbind(index) => {
                let requester = self.bindings[index]
                    .expect("reserved binding")
                    .context
                    .requester;
                if matches!(operation, Operation::Unbind(_)) {
                    self.storage.table.0.0[usize::from(requester)] = DENY;
                }
                Some(requester)
            }
            Operation::InstallAll => None,
        };
        if let Some(requester) = requester {
            commands[self.tail] = [(2 << 60) | u64::from(requester), 0];
            self.tail = (self.tail + 1) % commands.len();
        }
        if !matches!(operation, Operation::InstallRequester(_)) {
            commands[self.tail] = [8 << 60, 0]; // INVALIDATE_ALL, only with IASup.
            self.tail = (self.tail + 1) % commands.len();
        }
        // S=1, F=1: ordered completion store after invalidation.
        commands[self.tail] = [(1 << 60) | self.storage.completion.1.get() | 5, number];
        self.tail = (self.tail + 1) % commands.len();
        unsafe { self.storage.completion.0.0.get().write_volatile(0) };
        fence(Ordering::SeqCst);
        self.write(TAIL, (self.tail * 16) as u64);
    }

    /// Continue an original table installation or binding publication.
    pub fn poll(&mut self) -> Result<(), Error> {
        for _ in 0..256 {
            match self.progress {
                Progress::Starting => {
                    if self.read(CONTROL) != ENABLE {
                        return Err(self.uncertain());
                    }
                    if self.read(STATUS) & RUNNING != 0 {
                        self.next_install_batch()?;
                    }
                }
                Progress::Waiting(batch) => {
                    if matches!(batch.operation, Operation::Unbind(_)) {
                        return Err(Error::Occupied);
                    }
                    match self.observe_batch(batch) {
                        Ok(()) if matches!(batch.operation, Operation::InstallRequester(_)) => {
                            self.next_install_batch()?;
                            continue;
                        }
                        Ok(()) => {
                            self.progress = Progress::Ready;
                            return Ok(());
                        }
                        Err(Error::Pending) => {}
                        Err(error) => return Err(error),
                    }
                }
                Progress::Ready => return Ok(()),
                Progress::Unconfirmed(_) => return Err(Error::Unconfirmed),
                Progress::Fresh => return Err(Error::Occupied),
            }
            core::hint::spin_loop();
        }
        Err(Error::Pending)
    }

    pub(super) fn observe_batch(&mut self, batch: Batch) -> Result<(), Error> {
        let completion = unsafe { self.storage.completion.0.0.get().read_volatile() };
        if self.read(STATUS) & RUNNING == 0 {
            return Err(self.uncertain());
        }
        if completion != batch.number {
            return Err(Error::Pending);
        }
        fence(Ordering::Acquire);
        Ok(())
    }
}

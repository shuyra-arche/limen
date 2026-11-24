use core::{
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{
        AtomicU64,
        Ordering::{Acquire, Relaxed, Release},
    },
};

const NEXT: u64 = 1 << u32::BITS;

/// FIFO protection for execution publication while local interrupts are masked.
/// Each of the two participating Harts can hold or wait for one ticket.
pub(crate) struct Lock<T> {
    tickets: AtomicU64,
    value: UnsafeCell<T>,
}

// Every access to the value requires the guard for the serving ticket.
unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            tickets: AtomicU64::new(0),
            value: UnsafeCell::new(value),
        }
    }

    pub(crate) fn try_lock<'a>(&'a self, guards: &'a Cell<usize>) -> Option<Guard<'a, T>> {
        let word = self.tickets.load(Relaxed);
        let serving = word as u32;
        if (word >> u32::BITS) as u32 != serving {
            return None;
        }
        self.tickets
            .compare_exchange(word, word.wrapping_add(NEXT), Acquire, Relaxed)
            .ok()?;
        Some(self.guard(serving, guards))
    }

    pub(crate) fn lock<'a>(&'a self, guards: &'a Cell<usize>) -> Guard<'a, T> {
        self.wait(self.enqueue(guards), guards)
    }

    fn enqueue(&self, guards: &Cell<usize>) -> u32 {
        assert_eq!(
            guards.get(),
            0,
            "blocking acquisition while holding an ordinary Guard"
        );
        (self.tickets.fetch_add(NEXT, Relaxed) >> u32::BITS) as u32
    }

    fn wait<'a>(&'a self, ticket: u32, guards: &'a Cell<usize>) -> Guard<'a, T> {
        while self.tickets.load(Acquire) as u32 != ticket {
            core::hint::spin_loop();
        }
        self.guard(ticket, guards)
    }

    fn guard<'a>(&'a self, ticket: u32, guards: &'a Cell<usize>) -> Guard<'a, T> {
        guards.set(guards.get() + 1);
        Guard {
            lock: self,
            ticket,
            guards,
            local: PhantomData,
        }
    }
}

pub(crate) struct Guard<'a, T> {
    lock: &'a Lock<T>,
    ticket: u32,
    guards: &'a Cell<usize>,
    local: PhantomData<*mut ()>,
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        // When serving wraps, suppress the carry into the issuer half.
        let advance = if self.ticket == u32::MAX {
            1u64.wrapping_sub(NEXT)
        } else {
            1
        };
        self.lock.tickets.fetch_add(advance, Release);
        self.guards.set(self.guards.get() - 1);
    }
}

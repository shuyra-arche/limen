//! Interrupt masking and the switch gate for an admitted local CPU.

use core::{cell::Cell, marker::PhantomData};

use crate::metal::sync::local::LocalSync;

use super::Executor;
#[cfg(any(limen_target, test))]
use super::backend;

#[derive(Clone, Copy)]
struct Active {
    saved: usize,
    depth: usize,
}

pub(super) struct LocalProtection {
    active: Cell<Option<Active>>,
}

impl LocalProtection {
    #[cfg(any(limen_target, test))]
    pub(super) const fn new() -> Self {
        Self {
            active: Cell::new(None),
        }
    }
}

/// A protected window belonging to the current verified CPU.
#[must_use]
pub(in crate::metal) struct Protection {
    local: &'static Executor,
    same_hart: PhantomData<*mut ()>,
}

impl Protection {
    /// The fixed synchronization record associated at native admission.
    pub(in crate::metal) fn synchronization(&self) -> Option<&'static LocalSync> {
        self.local.synchronization.get().copied()
    }

    #[cfg(any(limen_target, test))]
    pub(in crate::metal::processor) fn is_outer(&self) -> bool {
        self.local
            .protection
            .active
            .get()
            .is_some_and(|active| active.depth == 1)
    }
}

#[cfg(any(limen_target, test))]
impl Executor {
    /// # Safety
    /// The caller has just verified this CPU's installed ordinary entry.
    pub(in crate::metal::processor) unsafe fn acquire_protection(&'static self) -> Protection {
        let saved = unsafe { backend::mask_interrupts() };
        let active = match self.protection.active.get() {
            Some(active) => Active {
                depth: active.depth.checked_add(1).expect("local protection depth"),
                ..active
            },
            None => Active { saved, depth: 1 },
        };
        self.protection.active.set(Some(active));
        Protection {
            local: self,
            same_hart: PhantomData,
        }
    }
}

impl Drop for Protection {
    fn drop(&mut self) {
        let state = &self.local.protection.active;
        let active = state.get().expect("active local protection");
        if active.depth == 1 {
            // Ordinary local operations are settled before IRQ can dispatch again.
            state.set(None);
            restore_interrupts(active.saved);
        } else {
            state.set(Some(Active {
                depth: active.depth - 1,
                ..active
            }));
        }
    }
}

#[cfg(any(limen_target, test))]
fn restore_interrupts(saved: usize) {
    // The first protection retains the native mask's original state.
    unsafe { backend::restore_interrupts(saved) };
}

#[cfg(not(any(limen_target, test)))]
fn restore_interrupts(_: usize) {
    unreachable!("interrupt restore requires a successful native mask");
}

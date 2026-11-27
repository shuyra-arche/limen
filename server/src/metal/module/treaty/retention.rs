//! The actual installed code use and its counted lifetime.

use super::super::installed::USE;
use super::{Contract, Machine, ModuleId};
use core::sync::atomic::{AtomicUsize, Ordering};
use limen_treaty::model::InternalAddress;

/// A nonduplicable use of one exact installed interface. Its actual consumer
/// must retain it through calls, continuations, and all cleanup. Transferring
/// the value transfers that retained use.
#[must_use = "keep this use while any consumer can reach the entry or its effects"]
pub struct ModuleUse<'module> {
    state: &'module AtomicUsize,
    entry: InternalAddress,
    contract: Contract<'module>,
    machine: Machine,
}

impl<'module> ModuleUse<'module> {
    pub(super) fn new(
        state: &'module AtomicUsize,
        entry: InternalAddress,
        contract: Contract<'module>,
        machine: Machine,
    ) -> Self {
        Self {
            state,
            entry,
            contract,
            machine,
        }
    }

    pub fn id(&self) -> ModuleId<'module> {
        ModuleId(self.state)
    }

    pub fn contract(&self) -> Contract<'module> {
        self.contract
    }

    pub fn machine(&self) -> Machine {
        self.machine
    }

    /// Internal entry coordinate for the consumer's previously validated domain ABI.
    /// Execution requires a live use and the corresponding execution scope.
    pub fn entry(&self) -> InternalAddress {
        self.entry
    }
}

impl Drop for ModuleUse<'_> {
    fn drop(&mut self) {
        self.state.fetch_sub(USE, Ordering::Release);
    }
}

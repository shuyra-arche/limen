//! Interface declarations and exact retained uses of installed modules.

use super::{ImageAddress, Machine, ModuleId, USE};
use crate::devicetree::NodeId;
use core::sync::atomic::{AtomicUsize, Ordering};
use limen_treaty::model::InternalAddress;

/// Exact interface declaration to match. The consumer defines what the
/// interface and calling ABI mean.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Contract<'a> {
    compatible: &'a [u8],
    abi: u32,
    version: u32,
}

impl<'a> Contract<'a> {
    pub const fn new(compatible: &'a [u8], abi: u32, version: u32) -> Self {
        Self {
            compatible,
            abi,
            version,
        }
    }

    pub const fn compatible(self) -> &'a [u8] {
        self.compatible
    }

    pub const fn abi(self) -> u32 {
        self.abi
    }

    pub const fn version(self) -> u32 {
        self.version
    }
}

/// An entry declaration tied to the original immutable note.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Interface<'a> {
    contract: Contract<'a>,
    entry: ImageAddress,
    source: NodeId<'a>,
}

impl<'a> Interface<'a> {
    pub(super) const fn new(
        contract: Contract<'a>,
        entry: ImageAddress,
        source: NodeId<'a>,
    ) -> Self {
        Self {
            contract,
            entry,
            source,
        }
    }

    pub const fn contract(self) -> Contract<'a> {
        self.contract
    }

    pub const fn version(self) -> u32 {
        self.contract.version()
    }

    /// Link-time virtual address in the file-backed executable segment.
    pub const fn entry(self) -> ImageAddress {
        self.entry
    }

    pub const fn source(self) -> NodeId<'a> {
        self.source
    }
}

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

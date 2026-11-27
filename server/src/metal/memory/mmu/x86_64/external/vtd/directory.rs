//! Resident root and context-page storage for one PCI bus.

use super::{
    binding::{Binding, Sequence},
    translation::Format,
};
use limen_treaty::model::PhysicalAddress;

/// One architectural root or context table. Both have 256 sixteen-byte entries.
#[repr(C, align(4096))]
pub struct Table(pub(super) [[u64; 2]; 256]);

impl Table {
    pub const EMPTY: Self = Self([[0; 2]; 256]);
}

/// Root and context-page custody for one admitted PCI bus. Hardware storage
/// and its independently retired descendants outlive outstanding effects.
pub struct Storage<'a> {
    pub bus: u8,
    pub root: (&'a mut Table, PhysicalAddress),
    pub contexts: (&'a mut Table, PhysicalAddress),
    pub bindings: &'a mut [Option<Binding>],
    pub sequence: &'a mut Sequence,
}

impl Format {
    pub(super) fn root_entry(self, context: PhysicalAddress) -> Option<[u64; 2]> {
        Some([self.page(context)? | 1, 0])
    }

    pub(super) fn context_entry(self, root: PhysicalAddress, domain: u16) -> Option<[u64; 2]> {
        // DID zero is reserved on caching-mode implementations.
        if domain == 0 || u32::from(domain) >= self.domain_limit {
            return None;
        }
        Some([self.page(root)? | 1, (u64::from(domain) << 8) | 1])
    }
}

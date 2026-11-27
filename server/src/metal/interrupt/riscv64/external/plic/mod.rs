//! Checked description and persistent serialized access to one physical PLIC.

use crate::metal::{
    interrupt::riscv64::external::Barrier,
    memory::mmu::access::device::DeviceWindow,
    platform::description::devicetree::{Error, Node, NodeId, cell},
};
use limen_treaty::model::{Alignment, PhysicalRange};

mod context;
mod registers;
mod route;
mod source;

pub use context::{Claim, ContextSlot, Destination, Receiver, State};
pub use route::{Route, RouteSlot, RouteState};
pub use source::{Source, SourceSlot};

use registers::{CONTEXTS, SOURCES, threshold};

/// The common checked description of a physical controller.
#[derive(Clone, Copy, Debug)]
pub struct Domain<'a> {
    node: NodeId<'a>,
    registers: PhysicalRange,
    sources: u32,
    contexts: usize,
}

impl<'a> Domain<'a> {
    fn read(node: Node<'_, 'a>) -> Result<Self, Error> {
        if !node.enabled()?
            || !(node.compatible(b"riscv,plic0")? || node.compatible(b"sifive,plic-1.0.0")?)
            || node.property(b"interrupt-controller")? != Some(b"")
            || cell(node.property(b"#interrupt-cells")?)? != Some(1)
        {
            return Err(Error::Unsupported);
        }
        let sources = cell(node.property(b"riscv,ndev")?)?.ok_or(Error::Missing)?;
        if !(1..=SOURCES).contains(&sources) {
            return Err(Error::Range);
        }
        let bytes = node
            .property(b"interrupts-extended")?
            .ok_or(Error::Missing)?;
        let (pairs, tail) = bytes.as_chunks::<8>();
        if pairs.is_empty() || !tail.is_empty() {
            return Err(Error::Format);
        }
        if pairs.len() > CONTEXTS {
            return Err(Error::Capacity);
        }
        let registers = node.register(0)?;
        if !registers.start().get().is_multiple_of(4096)
            || registers.length().get() < (threshold(pairs.len() - 1) + 8) as u64
        {
            return Err(Error::Range);
        }
        Ok(Self {
            node: node.id(),
            registers,
            sources,
            contexts: pairs.len(),
        })
    }

    pub fn registers(self) -> PhysicalRange {
        self.registers
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Capacity,
    Exhausted,
    Unconfirmed,
}

/// Legal programming range established for the admitted PLIC profile.
#[derive(Clone, Copy)]
pub struct Limits {
    priority: u32,
    threshold: u32,
}

impl Limits {
    pub fn new(priority_maximum: u32, threshold_maximum: u32) -> Option<Self> {
        (priority_maximum != 0).then_some(Self {
            priority: priority_maximum,
            threshold: threshold_maximum,
        })
    }

    fn check_priority(self, priority: u32) -> Result<(), Failure> {
        (priority != 0 && priority <= self.priority)
            .then_some(())
            .ok_or(Failure::Scope)
    }

    fn check_threshold(self, threshold: u32) -> Result<(), Failure> {
        (threshold <= self.threshold)
            .then_some(())
            .ok_or(Failure::Scope)
    }
}

/// The persistent owner of one physical domain. Composition serializes its
/// original contexts, shared enable words and global source priority writes.
#[must_use]
pub struct Controller<'storage, 'tree> {
    domain: Domain<'tree>,
    limits: Limits,
    contexts: &'storage mut [ContextSlot],
    sources: &'storage mut [SourceSlot],
    routes: &'storage mut [RouteSlot],
    next_round: u64,
}

/// Exclusive short-lived access using the executing Hart's own MMIO window.
/// Returning it leaves the original claims and programming records resident.
#[must_use]
pub struct Access<'access, 'storage, 'tree> {
    controller: &'access mut Controller<'storage, 'tree>,
    mapping: DeviceWindow<'access>,
    barrier: Barrier,
}

impl<'storage, 'tree> Controller<'storage, 'tree> {
    /// # Safety
    /// The caller owns this domain and excludes other writers in admitted
    /// contexts and admitted source priorities. Limits must come from the
    /// actual implementation's programmable values for this board profile.
    /// Retain authority and original records until all effects settle.
    pub unsafe fn admit(
        domain: Domain<'tree>,
        limits: Limits,
        contexts: &'storage mut [ContextSlot],
        sources: &'storage mut [SourceSlot],
        routes: &'storage mut [RouteSlot],
    ) -> Result<Self, Failure> {
        if contexts.is_empty()
            || contexts.len() > domain.contexts
            || sources.is_empty()
            || sources.len() > domain.sources as usize
            || routes.is_empty()
            || routes.len() > sources.len()
        {
            return Err(Failure::Scope);
        }
        if contexts.iter().any(ContextSlot::occupied)
            || sources.iter().any(SourceSlot::occupied)
            || routes.iter().any(RouteSlot::occupied)
        {
            return Err(Failure::Busy);
        }
        Ok(Self {
            domain,
            limits,
            contexts,
            sources,
            routes,
            next_round: 1,
        })
    }

    /// Borrow only the executing Hart's original MMIO window.
    ///
    /// # Safety
    /// An ordered enable-word readback completes preceding writes to that
    /// word, including mismatched values, on the actual interconnect. A
    /// later access may update the same word on another Hart.
    pub unsafe fn access<'access>(
        &'access mut self,
        mapping: &'access mut DeviceWindow<'_>,
        barrier: Barrier,
    ) -> Result<Access<'access, 'storage, 'tree>, Failure> {
        if self.domain.registers() != mapping.physical()
            || !mapping.is_aligned(Alignment::new(4096).unwrap())
        {
            return Err(Failure::Scope);
        }
        Ok(Access {
            controller: self,
            mapping: mapping.reborrow(),
            barrier,
        })
    }
}

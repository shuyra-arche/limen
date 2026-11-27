//! Device source wiring and the original global priority register.

use super::Domain;
use crate::metal::platform::description::devicetree::{DeviceTree, Error, Node, NodeId, cell};

#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    pub(super) device: NodeId<'a>,
    pub(super) domain: Domain<'a>,
    pub(super) id: u32,
}

impl<'a> Source<'a> {
    pub fn read(tree: &DeviceTree<'a>, device: Node<'_, 'a>) -> Result<Self, Error> {
        if tree.node(device.id()).is_none() {
            return Err(Error::Conflict);
        }
        if !device.enabled()? {
            return Err(Error::Missing);
        }
        if device.property(b"interrupts-extended")?.is_some() {
            return Err(Error::Unsupported);
        }
        let parent = cell(device.property(b"interrupt-parent")?)?.ok_or(Error::Missing)?;
        let domain = Domain::read(tree.phandle(parent)?.ok_or(Error::Missing)?)?;
        let id = cell(device.property(b"interrupts")?)?.ok_or(Error::Missing)?;
        if !(1..=domain.sources).contains(&id) {
            return Err(Error::Range);
        }
        Ok(Self {
            device: device.id(),
            domain,
            id,
        })
    }

    pub fn device(self) -> NodeId<'a> {
        self.device
    }

    pub fn number(self) -> u32 {
        self.id
    }

    pub fn controller(self) -> Domain<'a> {
        self.domain
    }
}

#[derive(Clone, Copy)]
pub(super) struct Priority {
    pub(super) id: u32,
    pub(super) inherited: u32,
    pub(super) requested: u32,
}

/// Resident storage for one physical source's priority obligation.
pub struct SourceSlot(pub(super) Option<Priority>);

impl SourceSlot {
    pub const EMPTY: Self = Self(None);

    pub(super) fn occupied(&self) -> bool {
        self.0.is_some()
    }
}

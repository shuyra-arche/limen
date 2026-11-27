//! Binding lookups within a retained flattened device tree.

use super::super::devicetree::MAX_DEPTH;

pub use super::super::devicetree::{DeviceTree, Error, Node, NodeId, Nodes, Property, memory};

impl<'a> DeviceTree<'a> {
    pub fn node(&self, id: NodeId<'a>) -> Option<Node<'_, 'a>> {
        if !id.belongs_to(self.bytes()) {
            return None;
        }
        self.nodes().find(|node| node.id() == id)
    }

    /// Locate an absolute path in this tree. Alias interpretation belongs to the caller.
    pub fn find_path(&self, path: &[u8]) -> Result<Option<Node<'_, 'a>>, Error> {
        let path = path.strip_prefix(b"/").ok_or(Error::Format)?;
        let mut node = self.nodes().next().expect("validated device tree root");
        if path.is_empty() {
            return Ok(Some(node));
        }
        for (depth, name) in path.split(|byte| *byte == b'/').enumerate() {
            if depth + 1 >= MAX_DEPTH {
                return Err(Error::Capacity);
            }
            if name.is_empty() {
                return Err(Error::Format);
            }
            let mut children = self
                .nodes()
                .filter(|child| child.parent_id() == Some(node.id()) && child.name() == name);
            let Some(child) = children.next() else {
                return Ok(None);
            };
            if children.next().is_some() {
                return Err(Error::Conflict);
            }
            node = child;
        }
        Ok(Some(node))
    }

    /// Resolve a phandle within this source and reject ambiguous references.
    pub fn phandle(&self, value: u32) -> Result<Option<Node<'_, 'a>>, Error> {
        if value == 0 || value == u32::MAX {
            return Err(Error::Format);
        }
        let mut found = None;
        for node in self.nodes() {
            if node.phandle()? == Some(value) && found.replace(node).is_some() {
                return Err(Error::Conflict);
            }
        }
        Ok(found)
    }
}

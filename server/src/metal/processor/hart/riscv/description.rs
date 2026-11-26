//! RISC-V CPU descriptions keep hardware IDs in their original tree scope.

use crate::metal::platform::firmware::devicetree::{DeviceTree, Error, Node, NodeId};

const HART_LIMIT: usize = 256;

/// Locate RISC-V Hart descriptions within the source tree's `/cpus` scope.
pub struct RiscvCpus<'a> {
    tree: DeviceTree<'a>,
    source: NodeId<'a>,
    address_cells: usize,
}

impl<'a> RiscvCpus<'a> {
    pub fn new(tree: DeviceTree<'a>) -> Result<Option<Self>, Error> {
        let Some(node) = tree.find_path(b"/cpus")? else {
            return Ok(None);
        };
        let address_cells = node.address_cells()?;
        if !matches!(address_cells, 1 | 2) || node.size_cells()? != 0 {
            return Err(Error::Unsupported);
        }
        let source = node.id();
        Ok(Some(Self {
            tree,
            source,
            address_cells,
        }))
    }

    pub fn node(&self) -> Node<'_, 'a> {
        self.tree.node(self.source).expect("original CPU node")
    }

    /// Locate a Hart by hardware ID. Validate the whole CPU identity scope
    /// before returning so duplicate or malformed siblings cannot be missed.
    pub fn cpu(&self, hart: u64) -> Result<Option<RiscvCpu<'_, 'a>>, Error> {
        let mut ids = [0; HART_LIMIT];
        let mut count = 0;
        let mut found = None;
        for node in self
            .tree
            .nodes()
            .filter(|node| node.parent_id() == Some(self.source))
        {
            let is_cpu_node = node.name() == b"cpu" || node.name().starts_with(b"cpu@");
            let cpu_type = node.string(b"device_type")?;
            if !is_cpu_node && cpu_type != Some(b"cpu") {
                continue;
            }
            if cpu_type != Some(b"cpu") {
                return Err(Error::Format);
            }
            let id = match (self.address_cells, node.property(b"reg")?) {
                (1, Some(value)) if value.len() == 4 => {
                    u64::from(u32::from_be_bytes(value.try_into().expect("four bytes")))
                }
                (2, Some(value)) if value.len() == 8 => {
                    u64::from_be_bytes(value.try_into().expect("eight bytes"))
                }
                (_, None) => return Err(Error::Missing),
                _ => return Err(Error::Format),
            };
            if ids[..count].contains(&id) {
                return Err(Error::Conflict);
            }
            *ids.get_mut(count).ok_or(Error::Capacity)? = id;
            count += 1;
            let enabled = node.enabled()?;
            if id == hart {
                found = Some(RiscvCpu { node, id, enabled });
            }
        }
        Ok(found)
    }
}

/// One described Hart, with a locator in the original device tree.
pub struct RiscvCpu<'tree, 'a> {
    node: Node<'tree, 'a>,
    id: u64,
    enabled: bool,
}

impl<'tree, 'a> RiscvCpu<'tree, 'a> {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Whether the description permits use. The hardware still confirms entry.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn node(&self) -> &Node<'tree, 'a> {
        &self.node
    }
}

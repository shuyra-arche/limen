//! RISC-V CPU descriptions keep hardware IDs in their original tree scope.

use crate::metal::platform::description::devicetree::{DeviceTree, Error, Node, NodeId};

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

    /// Resolve a CPU interrupt-controller phandle in the original CPU scope.
    /// A physical Hart ID is not a controller's dense target index.
    pub(crate) fn interrupt_hart(&self, handle: u32) -> Result<u64, Error> {
        let node = self.tree.phandle(handle)?.ok_or(Error::Missing)?;
        if !node.compatible(b"riscv,cpu-intc")?
            || node.property(b"interrupt-controller")? != Some(b"")
            || node.property(b"#interrupt-cells")? != Some(&1u32.to_be_bytes())
        {
            return Err(Error::Unsupported);
        }
        let parent = node.parent()?.ok_or(Error::Missing)?;
        if parent.parent_id() != Some(self.source) || parent.string(b"device_type")? != Some(b"cpu")
        {
            return Err(Error::Format);
        }
        let id = match (self.address_cells, parent.property(b"reg")?) {
            (1, Some(value)) if value.len() == 4 => {
                u64::from(u32::from_be_bytes(value.try_into().unwrap()))
            }
            (2, Some(value)) if value.len() == 8 => u64::from_be_bytes(value.try_into().unwrap()),
            _ => return Err(Error::Format),
        };
        if self
            .cpu(id)?
            .is_none_or(|cpu| cpu.node().id() != parent.id())
        {
            return Err(Error::Conflict);
        }
        Ok(id)
    }

    /// Check the complete homogeneous interrupts-extended list. The returned
    /// index follows controller wiring rather than the physical Hart number.
    pub(crate) fn interrupt_index(
        &self,
        bytes: &[u8],
        cause: u32,
        hart: u64,
    ) -> Result<Option<(usize, usize)>, Error> {
        let (pairs, remainder) = bytes.as_chunks::<8>();
        let count = pairs.len();
        if count == 0 || !remainder.is_empty() {
            return Err(Error::Format);
        }
        if count > HART_LIMIT {
            return Err(Error::Capacity);
        }
        let mut seen = [0; HART_LIMIT];
        let mut target = None;
        for (index, pair) in pairs.iter().enumerate() {
            let handle = u32::from_be_bytes(pair[..4].try_into().unwrap());
            let kind = u32::from_be_bytes(pair[4..].try_into().unwrap());
            if kind != cause {
                return Err(Error::Unsupported);
            }
            let id = self.interrupt_hart(handle)?;
            if seen[..index].contains(&id) {
                return Err(Error::Conflict);
            }
            seen[index] = id;
            if id == hart {
                target = Some((index, count));
            }
        }
        Ok(target)
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

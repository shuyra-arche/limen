use core::arch::asm;

use crate::metal::firmware::devicetree::{DeviceTree, Error, Node, NodeId};

const AFFINITY_MASK: u64 = 0xff00ffffff;
const CPU_LIMIT: usize = 64;

/// A processor described in the original tree. Its Hart must still enter
/// and confirm its own affinity before Limen treats it as ready.
#[derive(Clone, Copy, Debug)]
pub struct CpuCandidate<'a> {
    pub source: NodeId<'a>,
    pub affinity: usize,
    pub enabled: bool,
    pub starts_with_psci: bool,
}

/// The validated CPU affinity declarations from one device tree.
pub struct Aarch64Cpus<'tree, 'a> {
    tree: &'tree DeviceTree<'a>,
    parent: NodeId<'a>,
    cells: usize,
}

impl<'tree, 'a> Aarch64Cpus<'tree, 'a> {
    pub fn read(tree: &'tree DeviceTree<'a>) -> Result<Option<Self>, Error> {
        let Some(parent) = tree.find_path(b"/cpus")? else {
            return Ok(None);
        };
        let cells = parent.address_cells()?;
        if !matches!(cells, 1 | 2) || parent.size_cells()? != 0 {
            return Err(Error::Unsupported);
        }
        let cpus = Self {
            tree,
            parent: parent.id(),
            cells,
        };
        let mut affinities = [0; CPU_LIMIT];
        for (index, candidate) in cpus.entries().enumerate() {
            let affinity = candidate?.affinity;
            if index == affinities.len() {
                return Err(Error::Capacity);
            }
            if affinities[..index].contains(&affinity) {
                return Err(Error::Conflict);
            }
            affinities[index] = affinity;
        }
        Ok(Some(cpus))
    }

    fn entries(&self) -> impl Iterator<Item = Result<CpuCandidate<'a>, Error>> + '_ {
        self.tree
            .nodes()
            .filter(|node| node.parent_id() == Some(self.parent))
            .filter_map(|node| candidate(node, self.cells).transpose())
    }

    pub fn candidates(&self) -> impl Iterator<Item = CpuCandidate<'a>> + '_ {
        self.entries()
            .map(|entry| entry.expect("validated Arm CPU candidate"))
    }

    /// Find the enabled declaration belonging to the executing PE.
    ///
    /// # Safety
    /// Run in a privileged AArch64 context that can read MPIDR_EL1.
    pub unsafe fn current(&self) -> Result<CpuCandidate<'a>, Error> {
        let current = boot_identity();
        self.candidates()
            .find(|candidate| candidate.affinity == current && candidate.enabled)
            .ok_or(Error::Missing)
    }
}

fn candidate<'a>(node: Node<'_, 'a>, cells: usize) -> Result<Option<CpuCandidate<'a>>, Error> {
    match node.property(b"device_type")? {
        Some(b"cpu\0") => {}
        _ if node.name().starts_with(b"cpu@") => return Err(Error::Format),
        _ => return Ok(None),
    }
    let register = node.property(b"reg")?.ok_or(Error::Missing)?;
    let affinity = match cells {
        1 => u64::from(u32::from_be_bytes(
            register.try_into().map_err(|_| Error::Format)?,
        )),
        2 => u64::from_be_bytes(register.try_into().map_err(|_| Error::Format)?),
        _ => unreachable!("validated CPU address cell count"),
    };
    if affinity & !AFFINITY_MASK != 0 {
        return Err(Error::Range);
    }
    let affinity = usize::try_from(affinity).map_err(|_| Error::Range)?;
    let starts_with_psci = node.property(b"enable-method")? == Some(b"psci\0");
    Ok(Some(CpuCandidate {
        source: node.id(),
        affinity,
        enabled: node.enabled()?,
        starts_with_psci,
    }))
}

pub(crate) fn boot_identity() -> usize {
    let id: usize;
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) id, options(nomem,nostack)) };
    id & AFFINITY_MASK as usize
}

pub(super) fn local_address() -> usize {
    let address;
    unsafe { asm!("mrs {}, tpidr_el1", out(reg) address, options(nostack)) };
    address
}

pub(super) unsafe fn install_local(address: usize) {
    unsafe { asm!("msr tpidr_el1, {}", "isb", in(reg) address, options(nostack)) };
}

pub(super) unsafe fn mask_interrupts() -> usize {
    let previous;
    unsafe { asm!("mrs {}, daif", "msr daifset, #3", out(reg) previous, options(nostack)) };
    previous
}

pub(super) unsafe fn restore_interrupts(previous: usize) {
    unsafe { asm!("msr daif, {}", in(reg) previous, options(nostack)) };
}

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp
}

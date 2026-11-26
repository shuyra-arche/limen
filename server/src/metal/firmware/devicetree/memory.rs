use super::{DeviceTree, Error, Node, registers::Cells};
use crate::metal::fdt::RESERVATION_LIMIT;
#[cfg(any(limen_firmware = "sbi", test))]
use crate::metal::fdt::{HEADER_SIZE, MAX_BLOCK_BYTES, extent};
#[cfg(any(limen_firmware = "sbi", test))]
use core::{ops::Range, slice};
use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

const RAM_LIMIT: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    ReservationBlock,
    ReservedMemory { no_map: bool, reusable: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reservation {
    pub physical: PhysicalRange,
    pub source: Source,
}

/// The memory descriptions retained from a fully checked firmware tree.
#[derive(Debug)]
pub struct DtbMemory {
    ram: [Option<PhysicalRange>; RAM_LIMIT],
    ram_count: usize,
    reservations: [Option<Reservation>; RESERVATION_LIMIT],
    reservation_count: usize,
}

impl DtbMemory {
    const EMPTY: Self = Self {
        ram: [None; RAM_LIMIT],
        ram_count: 0,
        reservations: [None; RESERVATION_LIMIT],
        reservation_count: 0,
    };

    pub fn ram(&self) -> impl ExactSizeIterator<Item = PhysicalRange> + '_ {
        self.ram[..self.ram_count]
            .iter()
            .map(|entry| entry.expect("occupied RAM description"))
    }

    pub fn reservations(&self) -> impl ExactSizeIterator<Item = Reservation> + '_ {
        self.reservations[..self.reservation_count]
            .iter()
            .map(|entry| entry.expect("occupied memory reservation"))
    }

    fn add_ram(&mut self, region: PhysicalRange) -> Result<(), Error> {
        if self.ram().any(|other| region.overlaps(other)) {
            return Err(Error::Range);
        }
        *self.ram.get_mut(self.ram_count).ok_or(Error::Capacity)? = Some(region);
        self.ram_count += 1;
        Ok(())
    }

    fn add_reservation(&mut self, physical: PhysicalRange, source: Source) -> Result<(), Error> {
        *self
            .reservations
            .get_mut(self.reservation_count)
            .ok_or(Error::Capacity)? = Some(Reservation { physical, source });
        self.reservation_count += 1;
        Ok(())
    }
}

/// # Safety
///
/// The trusted platform provides a readable, identity-mapped window and
/// guarantees that the firmware's initialized DTB storage remains readable
/// and immutable for the returned view's lifetime. The DTB cannot supply any
/// part of that window bound.
#[cfg(any(limen_firmware = "sbi", test))]
pub(crate) unsafe fn read<'a>(
    input: *const (),
    window: Range<usize>,
    image: Range<usize>,
    memory: &mut Option<DtbMemory>,
) -> Result<DeviceTree<'a>, Error> {
    *memory = None;
    let start = input.addr();
    let header_end = start.checked_add(HEADER_SIZE).ok_or(Error::Window)?;
    if !window.contains(&start) || header_end > window.end {
        return Err(Error::Window);
    }
    let range = unsafe { extent(input.cast(), image) }.ok_or(Error::Format)?;
    if range.end > window.end {
        return Err(Error::Window);
    }
    if range.len() > MAX_BLOCK_BYTES {
        return Err(Error::Capacity);
    }
    // Only the exact DTB is borrowed. The enclosing window can contain
    // unrelated allocations, firmware storage and uninitialized bytes.
    let blob = unsafe { slice::from_raw_parts(input.cast(), range.len()) };
    let tree = DeviceTree::new(blob)?;
    describe(tree, memory)?;
    Ok(tree)
}

/// Populate the caller's fixed storage before it leaves the firmware adapter.
/// A failed decode removes the partial description.
pub fn describe(tree: DeviceTree<'_>, output: &mut Option<DtbMemory>) -> Result<(), Error> {
    describe_into(tree, output, true)
}

/// Retain the original reservations without using the tree as a RAM source.
pub fn describe_reservations(
    tree: DeviceTree<'_>,
    output: &mut Option<DtbMemory>,
) -> Result<(), Error> {
    describe_into(tree, output, false)
}

fn describe_into(
    tree: DeviceTree<'_>,
    output: &mut Option<DtbMemory>,
    include_ram: bool,
) -> Result<(), Error> {
    let result = parse(tree, output.insert(DtbMemory::EMPTY), include_ram);
    if result.is_err() {
        *output = None;
    }
    result
}

fn parse(tree: DeviceTree<'_>, memory: &mut DtbMemory, include_ram: bool) -> Result<(), Error> {
    for (base, size) in tree.reservations() {
        let physical = PhysicalAddress::new(base)
            .range(ByteLength::new(size))
            .ok_or(Error::Range)?;
        if memory
            .reservations()
            .any(|other| physical.overlaps(other.physical))
        {
            return Err(Error::Range);
        }
        memory.add_reservation(physical, Source::ReservationBlock)?;
    }

    let mut nodes = tree.nodes();
    let root = nodes.next().expect("validated device tree root");
    if include_ram {
        Cells::read(&root)?;
    }
    let mut reserved_node = None;
    for node in nodes {
        if node.parent_id() == Some(root.id()) {
            if node.name() == b"reserved-memory" {
                if reserved_node.is_some() {
                    return Err(Error::Conflict);
                }
                let enabled = enabled(&node)?;
                reserved_node = Some((node.id(), enabled));
                if enabled {
                    Cells::read(&node)?;
                }
                continue;
            }

            if !include_ram {
                continue;
            }

            let memory_node = node.name().split(|byte| *byte == b'@').next() == Some(b"memory");
            let memory_type = node.property(b"device_type")? == Some(b"memory\0");
            match (memory_node, memory_type) {
                (true, true) if enabled(&node)? => {
                    if node.property(b"reg")?.is_none() {
                        return Err(Error::Format);
                    }
                    for range in node.registers()? {
                        memory.add_ram(range?)?;
                    }
                }
                (true, false) => return Err(Error::Format),
                (false, true) => return Err(Error::Unsupported),
                _ => (),
            }
        } else if let Some((reserved_id, true)) = reserved_node
            && node.parent_id() == Some(reserved_id)
            && enabled(&node)?
        {
            describe_reservation(&node, memory)?;
        }
    }
    if include_ram && memory.ram_count == 0 {
        return Err(Error::Format);
    }
    Ok(())
}

// The memory binding defines an explicit disabled status. Other statuses
// leave the allocation or reservation effect unknown.
fn enabled(node: &Node<'_, '_>) -> Result<bool, Error> {
    match node.property(b"status")? {
        None | Some(b"okay\0") => Ok(true),
        Some(b"disabled\0") => Ok(false),
        Some(_) => Err(Error::Unsupported),
    }
}

fn describe_reservation(node: &Node<'_, '_>, memory: &mut DtbMemory) -> Result<(), Error> {
    let no_map = node.property(b"no-map")?;
    let reusable = node.property(b"reusable")?;
    if no_map.is_some_and(|bytes| !bytes.is_empty())
        || reusable.is_some_and(|bytes| !bytes.is_empty())
        || (no_map.is_some() && reusable.is_some())
    {
        return Err(Error::Format);
    }
    if node.property(b"reg")?.is_none() {
        return Err(if node.property(b"size")?.is_some() {
            Error::Unsupported
        } else {
            Error::Format
        });
    }
    let source = Source::ReservedMemory {
        no_map: no_map.is_some(),
        reusable: reusable.is_some(),
    };
    for range in node.registers()? {
        memory.add_reservation(range?, source)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../../tests/metal/firmware/devicetree/retained.rs"]
mod tests;

use super::{Error, Node};
use limen_treaty::model::{ByteCoordinate, ByteLength, PhysicalAddress, PhysicalRange, Range};

// Each intermediate range is interpreted in the current parent bus.
enum Bus {}
type BusAddress = ByteCoordinate<Bus>;
type BusRange = Range<BusAddress>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Cells {
    pub(super) address: usize,
    pub(super) size: usize,
}

impl Cells {
    pub(super) fn read(node: &Node<'_, '_>) -> Result<Self, Error> {
        let cells = Self {
            address: node.address_cells()?,
            size: node.size_cells()?,
        };
        if !(1..=2).contains(&cells.address) || !(1..=2).contains(&cells.size) {
            return Err(Error::Unsupported);
        }
        let Some(root) = node.parent()? else {
            return Ok(cells);
        };
        // A cell count alone cannot distinguish scalar addresses from a
        // binding with address-space flags, such as ISA I/O space.
        if node.name() == b"reserved-memory" {
            if root.parent_id().is_some()
                || node.property(b"#address-cells")?.is_none()
                || node.property(b"#size-cells")?.is_none()
                || node.property(b"ranges")? != Some(b"")
                || cells != Self::read(&root)?
            {
                return Err(Error::Unsupported);
            }
        } else if !node.compatible(b"simple-bus")? {
            return Err(Error::Unsupported);
        }
        Ok(cells)
    }

    fn decode(self, tuple: &[u8]) -> Result<BusRange, Error> {
        let (base, length) = tuple.split_at(self.address * 4);
        let range = BusAddress::new(number(base))
            .range(ByteLength::new(number(length)))
            .ok_or(Error::Range)?;
        within_address_cells(range, self.address)
    }
}

impl<'a> Node<'_, 'a> {
    /// The number of 32-bit address cells for direct children.
    pub fn address_cells(&self) -> Result<usize, Error> {
        self.cell_count(b"#address-cells", 2)
    }

    /// The number of 32-bit size cells for direct children.
    pub fn size_cells(&self) -> Result<usize, Error> {
        self.cell_count(b"#size-cells", 1)
    }

    fn cell_count(&self, name: &[u8], default: usize) -> Result<usize, Error> {
        match self.property(name)? {
            Some(bytes) => {
                let word = <[u8; 4]>::try_from(bytes).map_err(|_| Error::Format)?;
                Ok(u32::from_be_bytes(word) as usize)
            }
            None => Ok(default),
        }
    }

    /// Resolve a byte-addressable register through supported scalar address
    /// domains: the root, simple buses and the top-level reserved-memory node.
    pub fn register(&self, index: usize) -> Result<PhysicalRange, Error> {
        self.registers()?.nth(index).ok_or(Error::Missing)?
    }

    pub fn registers(
        &self,
    ) -> Result<impl ExactSizeIterator<Item = Result<PhysicalRange, Error>> + '_, Error> {
        let bus = self.parent()?.ok_or(Error::Missing)?;
        let cells = Cells::read(&bus)?;
        let bytes = self.property(b"reg")?.ok_or(Error::Missing)?;
        let tuples = bytes.chunks_exact((cells.address + cells.size) * 4);
        if tuples.len() == 0 || !tuples.remainder().is_empty() {
            return Err(Error::Format);
        }
        Ok(tuples.map(move |tuple| {
            let mut range = cells.decode(tuple)?;
            let mut bus = bus;
            while let Some(parent) = bus.parent()? {
                range = translate(&bus, &parent, range)?;
                bus = parent;
            }
            // The complete chain reached the physical root of this tree.
            PhysicalAddress::new(range.start().get())
                .range(range.length())
                .ok_or(Error::Range)
        }))
    }
}

fn translate(
    bus: &Node<'_, '_>,
    parent: &Node<'_, '_>,
    range: BusRange,
) -> Result<BusRange, Error> {
    let child = Cells::read(bus)?;
    let ranges = bus.property(b"ranges")?.ok_or(Error::Missing)?;
    let parent_address = parent.address_cells()?;
    if !(1..=2).contains(&parent_address) {
        return Err(Error::Unsupported);
    }
    if ranges.is_empty() {
        return within_address_cells(range, parent_address);
    }

    let tuples = ranges.chunks_exact((child.address + parent_address + child.size) * 4);
    if !tuples.remainder().is_empty() {
        return Err(Error::Format);
    }
    let mut matched = None;
    let mut overlaps = 0;
    for tuple in tuples {
        let (child_base, rest) = tuple.split_at(child.address * 4);
        let (parent_base, size) = rest.split_at(parent_address * 4);
        let extent = ByteLength::new(number(size));
        let child_range = BusAddress::new(number(child_base))
            .range(extent)
            .ok_or(Error::Range)?;
        within_address_cells(child_range, child.address)?;
        let parent_range = BusAddress::new(number(parent_base))
            .range(extent)
            .ok_or(Error::Range)?;
        within_address_cells(parent_range, parent_address)?;
        if child_range.overlaps(range) {
            overlaps += 1;
        }
        if child_range.covers(range) {
            let offset = range
                .start()
                .distance(child_range.start())
                .ok_or(Error::Range)?;
            let base = parent_range.start().advance(offset).ok_or(Error::Range)?;
            matched = Some(base.range(range.length()).ok_or(Error::Range)?);
        }
    }
    if overlaps > 1 {
        return Err(Error::Conflict);
    }
    matched.ok_or(if overlaps == 0 {
        Error::Missing
    } else {
        Error::Range
    })
}

fn within_address_cells(range: BusRange, address_cells: usize) -> Result<BusRange, Error> {
    if address_cells == 1 && range.end().get() > 1_u64 << 32 {
        return Err(Error::Range);
    }
    Ok(range)
}

fn number(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0, |value, byte| (value << 8) | u64::from(*byte))
}

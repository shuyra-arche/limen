#[cfg(any(target_arch = "riscv64", test))]
use super::extent;
use super::{HEADER_SIZE, layout};
#[cfg(any(target_arch = "riscv64", test))]
use core::{ops::Range, slice};

const MAX_BLOCK_BYTES: usize = 256 * 1024;
// AAVMF retains the QEMU tree with free space and an extra EFI page.
const MAX_EXTENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const RAM_LIMIT: usize = 16;
const RESERVATION_LIMIT: usize = 64;

const BEGIN: u32 = 1;
const END_NODE: u32 = 2;
const PROPERTY: u32 = 3;
const NOP: u32 = 4;
const END: u32 = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Physical {
    pub start: u64,
    pub end: u64,
}

impl Physical {
    fn new(start: u64, size: u64) -> Result<Self, Error> {
        let end = start.checked_add(size).ok_or(Error::Range)?;
        if start == end {
            return Err(Error::Range);
        }
        Ok(Self { start, end })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    ReservationBlock,
    ReservedMemory { no_map: bool, reusable: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reservation {
    pub physical: Physical,
    pub source: Source,
}

/// The memory descriptions retained from a fully checked firmware tree.
#[derive(Debug)]
pub struct DtbMemory {
    ram: [Physical; RAM_LIMIT],
    ram_count: usize,
    reservations: [Reservation; RESERVATION_LIMIT],
    reservation_count: usize,
}

impl DtbMemory {
    const EMPTY: Self = Self {
        ram: [Physical { start: 0, end: 0 }; RAM_LIMIT],
        ram_count: 0,
        reservations: [Reservation {
            physical: Physical { start: 0, end: 0 },
            source: Source::ReservationBlock,
        }; RESERVATION_LIMIT],
        reservation_count: 0,
    };

    pub fn ram(&self) -> &[Physical] {
        &self.ram[..self.ram_count]
    }

    pub fn reservations(&self) -> &[Reservation] {
        &self.reservations[..self.reservation_count]
    }

    fn add_ram(&mut self, region: Physical) -> Result<(), Error> {
        if self
            .ram()
            .iter()
            .any(|other| region.start < other.end && other.start < region.end)
        {
            return Err(Error::Range);
        }
        *self.ram.get_mut(self.ram_count).ok_or(Error::Capacity)? = region;
        self.ram_count += 1;
        Ok(())
    }

    fn add_reservation(&mut self, physical: Physical, source: Source) -> Result<(), Error> {
        *self
            .reservations
            .get_mut(self.reservation_count)
            .ok_or(Error::Capacity)? = Reservation { physical, source };
        self.reservation_count += 1;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    #[cfg(any(target_arch = "riscv64", test))]
    Window,
    Format,
    Range,
    Capacity,
    Unsupported,
}

/// # Safety
///
/// The trusted platform provides a readable, identity-mapped window and
/// guarantees that the firmware's DTB storage is initialized and immutable
/// for the reads below. The DTB cannot supply any part of that window bound.
#[cfg(any(target_arch = "riscv64", test))]
pub(crate) unsafe fn read(
    input: *const (),
    window: Range<usize>,
    image: Range<usize>,
) -> Result<(Range<usize>, DtbMemory), Error> {
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
    let mut memory = None;
    describe(blob, &mut memory)?;
    Ok((range, memory.expect("described DTB memory")))
}

/// Populate the caller's fixed storage before it leaves the firmware adapter.
/// A failed decode removes the partial description.
pub(crate) fn describe(blob: &[u8], output: &mut Option<DtbMemory>) -> Result<(), Error> {
    let result = parse(blob, output.insert(DtbMemory::EMPTY));
    if result.is_err() {
        *output = None;
    }
    result
}

fn parse(blob: &[u8], memory: &mut DtbMemory) -> Result<(), Error> {
    if blob.len() > MAX_EXTENT_BYTES {
        return Err(Error::Capacity);
    }
    let header = blob.first_chunk::<HEADER_SIZE>().ok_or(Error::Format)?;
    let mut words = [0; 10];
    for (word, raw) in words.iter_mut().zip(header.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*raw);
    }
    let layout = layout(words).ok_or(Error::Format)?;
    if layout.size as usize != blob.len() {
        return Err(Error::Format);
    }
    // Free space in the retained extent never enters the structure or string
    // scan. The reservation walk has its own fixed entry limit.
    if layout.structure.len() + layout.strings.len() > MAX_BLOCK_BYTES {
        return Err(Error::Capacity);
    }

    let start = layout.reservations;
    // The terminator must lie wholly before the next block.
    let edge = [&layout.structure, &layout.strings]
        .into_iter()
        .filter(|block| !block.is_empty() && block.start > start)
        .map(|block| block.start)
        .min()
        .unwrap_or(layout.size);
    let mut reserved = &blob[start as usize..edge as usize];
    loop {
        let (pair, rest) = reserved.split_first_chunk::<16>().ok_or(Error::Format)?;
        reserved = rest;
        let base = u64::from_be_bytes(*pair.first_chunk::<8>().ok_or(Error::Format)?);
        let size = u64::from_be_bytes(*pair.last_chunk::<8>().ok_or(Error::Format)?);
        if base == 0 && size == 0 {
            break;
        }
        let physical = Physical::new(base, size)?;
        if memory
            .reservations()
            .iter()
            .any(|other| physical.start < other.physical.end && other.physical.start < physical.end)
        {
            return Err(Error::Range);
        }
        memory.add_reservation(physical, Source::ReservationBlock)?;
    }

    let mut tree = Tree {
        structure: &blob[layout.structure.start as usize..layout.structure.end as usize],
        strings: &blob[layout.strings.start as usize..layout.strings.end as usize],
    };
    if tree.token()? != BEGIN || !tree.name()?.is_empty() {
        return Err(Error::Format);
    }
    let root = tree.properties()?;
    let cells = Cells::read(&root)?;
    let mut reserved_node = false;
    while let Some(name) = tree.child()? {
        let properties = tree.properties()?;
        if name == b"reserved-memory" {
            if reserved_node {
                return Err(Error::Format);
            }
            reserved_node = true;
            if properties.enabled()? {
                tree.reservations(properties, &cells, memory)?;
            } else {
                tree.skip(2)?;
            }
            continue;
        }
        if name.split(|byte| *byte == b'@').next() == Some(b"memory") {
            if properties.device_type != Some(b"memory\0") {
                return Err(Error::Format);
            }
            if properties.enabled()? {
                cells.reg(properties.reg, |physical| memory.add_ram(physical))?;
            }
        } else if properties.device_type == Some(b"memory\0") {
            return Err(Error::Unsupported);
        }
        tree.skip(2)?;
    }
    if tree.token()? != END || !tree.structure.is_empty() || memory.ram().is_empty() {
        return Err(Error::Format);
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
struct Cells {
    address: usize,
    size: usize,
}

impl Cells {
    fn read(properties: &Properties<'_>) -> Result<Self, Error> {
        fn width(value: Option<&[u8]>, default: usize) -> Result<usize, Error> {
            match value {
                None => Ok(default),
                Some([0, 0, 0, n @ (1 | 2)]) => Ok(usize::from(*n) * 4),
                Some(_) => Err(Error::Unsupported),
            }
        }
        Ok(Self {
            address: width(properties.address_cells, 8)?,
            size: width(properties.size_cells, 4)?,
        })
    }

    fn reg(
        &self,
        bytes: Option<&[u8]>,
        mut record: impl FnMut(Physical) -> Result<(), Error>,
    ) -> Result<(), Error> {
        fn number(bytes: &[u8]) -> u64 {
            bytes
                .iter()
                .fold(0, |value, byte| (value << 8) | u64::from(*byte))
        }
        let bytes = bytes
            .filter(|bytes| !bytes.is_empty())
            .ok_or(Error::Format)?;
        let tuples = bytes.chunks_exact(self.address + self.size);
        if !tuples.remainder().is_empty() {
            return Err(Error::Format);
        }
        for tuple in tuples {
            let (address, size) = tuple.split_at(self.address);
            record(Physical::new(number(address), number(size))?)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Properties<'a> {
    address_cells: Option<&'a [u8]>,
    size_cells: Option<&'a [u8]>,
    device_type: Option<&'a [u8]>,
    reg: Option<&'a [u8]>,
    ranges: Option<&'a [u8]>,
    status: Option<&'a [u8]>,
    size: Option<&'a [u8]>,
    no_map: Option<&'a [u8]>,
    reusable: Option<&'a [u8]>,
}

impl Properties<'_> {
    fn enabled(&self) -> Result<bool, Error> {
        match self.status {
            None | Some(b"okay\0") => Ok(true),
            Some(b"disabled\0") => Ok(false),
            Some(_) => Err(Error::Unsupported),
        }
    }
}

struct Tree<'a> {
    structure: &'a [u8],
    strings: &'a [u8],
}

impl<'a> Tree<'a> {
    fn word(&mut self) -> Result<u32, Error> {
        let (word, rest) = self
            .structure
            .split_first_chunk::<4>()
            .ok_or(Error::Format)?;
        self.structure = rest;
        Ok(u32::from_be_bytes(*word))
    }

    fn token(&mut self) -> Result<u32, Error> {
        loop {
            let token = self.word()?;
            if token != NOP {
                return Ok(token);
            }
        }
    }

    fn aligned(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let padded = length.checked_next_multiple_of(4).ok_or(Error::Format)?;
        let (bytes, rest) = self
            .structure
            .split_at_checked(padded)
            .ok_or(Error::Format)?;
        self.structure = rest;
        Ok(&bytes[..length])
    }

    fn name(&mut self) -> Result<&'a [u8], Error> {
        let size = self
            .structure
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(Error::Format)?;
        Ok(&self.aligned(size + 1)?[..size])
    }

    fn properties(&mut self) -> Result<Properties<'a>, Error> {
        let mut properties = Properties::default();
        loop {
            let pending = self.structure;
            match self.word()? {
                NOP => continue,
                PROPERTY => (),
                _ => {
                    self.structure = pending;
                    return Ok(properties);
                }
            }
            let length = self.word()? as usize;
            let offset = self.word()? as usize;
            let data = self.aligned(length)?;
            let string = self.strings.get(offset..).ok_or(Error::Format)?;
            // Property name length is limited by the Devicetree format.
            let length = string
                .iter()
                .take(32)
                .position(|byte| *byte == 0)
                .ok_or(Error::Format)?;
            let field = match &string[..length] {
                b"#address-cells" => &mut properties.address_cells,
                b"#size-cells" => &mut properties.size_cells,
                b"device_type" => &mut properties.device_type,
                b"reg" => &mut properties.reg,
                b"ranges" => &mut properties.ranges,
                b"status" => &mut properties.status,
                b"size" => &mut properties.size,
                b"no-map" => &mut properties.no_map,
                b"reusable" => &mut properties.reusable,
                [] => return Err(Error::Format),
                _ => continue,
            };
            if field.replace(data).is_some() {
                return Err(Error::Format);
            }
        }
    }

    fn child(&mut self) -> Result<Option<&'a [u8]>, Error> {
        match self.token()? {
            BEGIN => {
                let name = self.name()?;
                if name.is_empty() {
                    return Err(Error::Format);
                }
                Ok(Some(name))
            }
            END_NODE => Ok(None),
            _ => Err(Error::Format),
        }
    }

    fn skip(&mut self, initial: usize) -> Result<(), Error> {
        let mut depth = initial;
        loop {
            if self.child()?.is_some() {
                if depth == MAX_DEPTH {
                    return Err(Error::Capacity);
                }
                self.properties()?;
                depth += 1;
            } else if depth == initial {
                return Ok(());
            } else {
                depth -= 1;
            }
        }
    }

    fn reservations(
        &mut self,
        parent: Properties<'_>,
        cells: &Cells,
        memory: &mut DtbMemory,
    ) -> Result<(), Error> {
        if parent.address_cells.is_none()
            || parent.size_cells.is_none()
            || Cells::read(&parent)? != *cells
            || parent.ranges != Some(b"")
        {
            return Err(Error::Unsupported);
        }
        while self.child()?.is_some() {
            let properties = self.properties()?;
            if properties.enabled()? {
                let no_map = properties.no_map.is_some();
                let reusable = properties.reusable.is_some();
                if properties.no_map.is_some_and(|bytes| !bytes.is_empty())
                    || properties.reusable.is_some_and(|bytes| !bytes.is_empty())
                    || (no_map && reusable)
                {
                    return Err(Error::Format);
                }
                if properties.reg.is_none() && properties.size.is_some() {
                    return Err(Error::Unsupported);
                }
                let source = Source::ReservedMemory { no_map, reusable };
                cells.reg(properties.reg, |physical| {
                    memory.add_reservation(physical, source)
                })?;
            }
            self.skip(3)?;
        }
        Ok(())
    }
}

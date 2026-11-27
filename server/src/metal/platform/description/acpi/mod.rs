//! Static ACPI descriptions under an independently retained physical read scope.

use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

mod dmar;
mod ivrs;
pub(crate) use ivrs::DeviceSelections;
pub use ivrs::Ivhd;
mod madt;
pub use dmar::{Dmar, RemappingUnit};
pub use madt::{IoApic, Madt, Polarity, Processor, Record, SourceOverride, Trigger};

const RSDP_PREFIX: usize = 20;
const RSDP_HEADER: usize = 36;
const TABLE_HEADER: usize = 36;
const ADDRESS_BYTES: usize = size_of::<u64>();
const TABLE_LIMIT: usize = 24;
const TABLE_BYTES: usize = 256 * 1024;
const TOTAL_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidAddress,
    Unreadable(PhysicalRange),
    InvalidSignature,
    InvalidLength,
    InvalidChecksum,
    UnsupportedRevision,
    InconsistentRead,
    Capacity,
    Overlap,
    DuplicateTable,
    InvalidRecord,
    DuplicateRecord,
}

/// One checksum-verified system description. The physical range names its source.
#[derive(Clone, Copy, Debug)]
pub struct Table<'a> {
    start: PhysicalAddress,
    bytes: &'a [u8],
}

impl<'a> Table<'a> {
    pub fn physical(self) -> PhysicalRange {
        self.start
            .range(ByteLength::new(self.bytes.len() as u64))
            .expect("validated ACPI table extent")
    }

    pub fn signature(self) -> [u8; 4] {
        field(self.bytes, 0)
    }

    pub fn revision(self) -> u8 {
        self.bytes[8]
    }

    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

/// Retained ACPI 2.0+ root and the independently validated XSDT entries.
#[derive(Debug)]
pub struct Acpi<'a> {
    rsdp: PhysicalRange,
    xsdt: Table<'a>,
    entries: [&'a [u8]; TABLE_LIMIT],
    madt: Option<Madt<'a>>,
}

impl<'a> Acpi<'a> {
    /// Interpret stable bytes returned for each independently checked physical range.
    ///
    /// `read` must return exactly the requested immutable bytes in physical order,
    /// or `None` for a range without a retained, readable mapping. A table's own
    /// address, length or checksum cannot establish the read scope for another table.
    pub fn new(
        rsdp: PhysicalAddress,
        mut read: impl FnMut(PhysicalRange) -> Option<&'a [u8]>,
    ) -> Result<Self, Error> {
        let (rsdp, xsdt) = read_rsdp(rsdp, &mut read)?;
        let xsdt = read_table(
            xsdt,
            TABLE_HEADER + ADDRESS_BYTES * TABLE_LIMIT,
            |range| rsdp.overlaps(range),
            &mut read,
        )?;
        if xsdt.signature() != *b"XSDT" {
            return Err(Error::InvalidSignature);
        }
        if xsdt.revision() != 1 {
            return Err(Error::UnsupportedRevision);
        }
        if !(xsdt.bytes.len() - TABLE_HEADER).is_multiple_of(ADDRESS_BYTES) {
            return Err(Error::InvalidLength);
        }
        let mut acpi = Self {
            rsdp,
            xsdt,
            entries: [&[]; TABLE_LIMIT],
            madt: None,
        };
        let root_bytes = rsdp.length().get();
        let mut remaining = TOTAL_BYTES - root_bytes as usize - xsdt.bytes.len();
        for (index, start) in addresses(xsdt).enumerate() {
            let table = read_table(
                start,
                remaining.min(TABLE_BYTES),
                |range| {
                    rsdp.overlaps(range)
                        || xsdt.physical().overlaps(range)
                        || acpi
                            .tables()
                            .take(index)
                            .any(|prior| prior.physical().overlaps(range))
                },
                &mut read,
            )?;
            if table.signature() == *b"APIC" {
                if acpi.madt.is_some() {
                    return Err(Error::DuplicateTable);
                }
                acpi.madt = Some(Madt::new(table)?);
            }
            remaining -= table.bytes.len();
            acpi.entries[index] = table.bytes;
        }
        Ok(acpi)
    }

    pub const fn rsdp_range(&self) -> PhysicalRange {
        self.rsdp
    }

    pub const fn xsdt(&self) -> Table<'a> {
        self.xsdt
    }

    /// Enumerate source-scoped entries, including those without a supported binding.
    pub fn tables(&self) -> impl Iterator<Item = Table<'a>> + '_ {
        addresses(self.xsdt)
            .zip(self.entries.iter().copied())
            .map(|(start, bytes)| Table { start, bytes })
    }

    pub const fn madt(&self) -> Option<Madt<'a>> {
        self.madt
    }
}

fn read_rsdp<'a>(
    address: PhysicalAddress,
    read: &mut impl FnMut(PhysicalRange) -> Option<&'a [u8]>,
) -> Result<(PhysicalRange, PhysicalAddress), Error> {
    let prefix = read_exact(physical_range(address, RSDP_PREFIX)?, read)?;
    if field::<8>(prefix, 0) != *b"RSD PTR " {
        return Err(Error::InvalidSignature);
    }
    checksum(prefix)?;
    if prefix[15] < 2 {
        return Err(Error::UnsupportedRevision);
    }
    let header = read_exact(physical_range(address, RSDP_HEADER)?, read)?;
    if header.get(..RSDP_PREFIX) != Some(prefix) {
        return Err(Error::InconsistentRead);
    }
    let length = u32::from_le_bytes(field(header, 20)) as usize;
    if length < RSDP_HEADER {
        return Err(Error::InvalidLength);
    }
    if length > 4096 {
        return Err(Error::Capacity);
    }
    let physical = physical_range(address, length)?;
    let bytes = if length == RSDP_HEADER {
        header
    } else {
        let bytes = read_exact(physical, read)?;
        if bytes.get(..RSDP_HEADER) != Some(header) {
            return Err(Error::InconsistentRead);
        }
        bytes
    };
    checksum(bytes)?;
    let xsdt = PhysicalAddress::new(u64::from_le_bytes(field(bytes, 24)));
    Ok((physical, xsdt))
}

fn read_table<'a>(
    address: PhysicalAddress,
    maximum: usize,
    overlaps: impl Fn(PhysicalRange) -> bool,
    read: &mut impl FnMut(PhysicalRange) -> Option<&'a [u8]>,
) -> Result<Table<'a>, Error> {
    if maximum < TABLE_HEADER {
        return Err(Error::Capacity);
    }
    let prefix = physical_range(address, TABLE_HEADER)?;
    if overlaps(prefix) {
        return Err(Error::Overlap);
    }
    let header = read_exact(prefix, read)?;
    if !header[..4]
        .iter()
        .all(|byte| byte.is_ascii() && !byte.is_ascii_control())
    {
        return Err(Error::InvalidSignature);
    }
    let length = u32::from_le_bytes(field(header, 4)) as usize;
    if length < TABLE_HEADER {
        return Err(Error::InvalidLength);
    }
    if length > maximum {
        return Err(Error::Capacity);
    }
    let physical = physical_range(address, length)?;
    if overlaps(physical) {
        return Err(Error::Overlap);
    }
    let bytes = if length == TABLE_HEADER {
        header
    } else {
        let bytes = read_exact(physical, read)?;
        if bytes.get(..TABLE_HEADER) != Some(header) {
            return Err(Error::InconsistentRead);
        }
        bytes
    };
    checksum(bytes)?;
    Ok(Table {
        start: address,
        bytes,
    })
}

fn read_exact<'a>(
    range: PhysicalRange,
    read: &mut impl FnMut(PhysicalRange) -> Option<&'a [u8]>,
) -> Result<&'a [u8], Error> {
    let count = range.length().get();
    read(range)
        .filter(|bytes| bytes.len() as u64 == count)
        .ok_or(Error::Unreadable(range))
}

fn physical_range(address: PhysicalAddress, length: usize) -> Result<PhysicalRange, Error> {
    if address.get() == 0 {
        return Err(Error::InvalidAddress);
    }
    address
        .range(ByteLength::new(length as u64))
        .ok_or(Error::InvalidAddress)
}

fn addresses(xsdt: Table<'_>) -> impl Iterator<Item = PhysicalAddress> + '_ {
    xsdt.bytes[TABLE_HEADER..]
        .as_chunks::<ADDRESS_BYTES>()
        .0
        .iter()
        .map(|bytes| PhysicalAddress::new(u64::from_le_bytes(*bytes)))
}

fn checksum(bytes: &[u8]) -> Result<(), Error> {
    if bytes.iter().copied().fold(0u8, u8::wrapping_add) != 0 {
        return Err(Error::InvalidChecksum);
    }
    Ok(())
}

fn field<const SIZE: usize>(bytes: &[u8], start: usize) -> [u8; SIZE] {
    bytes[start..start + SIZE]
        .try_into()
        .expect("validated ACPI field extent")
}

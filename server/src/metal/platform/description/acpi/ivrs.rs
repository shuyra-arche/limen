//! A checked IVHD register description for a single physical AMD IOMMU.
//! Device entries remain tied to the original retained firmware table.

use super::{Acpi, Error, field};
use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ivhd<'a> {
    segment: u16,
    device: u16,
    capability: u16,
    registers: PhysicalRange,
    entries: &'a [u8],
}

impl Ivhd<'_> {
    pub fn segment(&self) -> u16 {
        self.segment
    }

    pub fn device(&self) -> u16 {
        self.device
    }

    pub fn capability(&self) -> u16 {
        self.capability
    }

    pub fn registers(&self) -> PhysicalRange {
        self.registers
    }

    /// The select-only scope is sufficient for directly attached PCI functions.
    /// Aliases, ranges and nonzero firmware settings require another profile.
    pub fn selects(&self, device: u16) -> bool {
        let mut rest = self.entries;
        while !rest.is_empty() {
            let (entry, next) = split_entry(rest).expect("validated IVHD entries");
            if entry[0] == 2 && u16::from_le_bytes(field(entry, 1)) == device {
                return true;
            }
            rest = next;
        }
        false
    }
}

impl<'a> Acpi<'a> {
    pub fn ivhd(&self) -> Result<Option<Ivhd<'a>>, Error> {
        let mut tables = self.tables().filter(|table| table.signature() == *b"IVRS");
        let Some(table) = tables.next() else {
            return Ok(None);
        };
        if tables.next().is_some() {
            return Err(Error::DuplicateTable);
        }
        if table.revision() != 1 {
            return Err(Error::UnsupportedRevision);
        }
        decode(table.bytes()).map(Some)
    }
}

fn split_entry(bytes: &[u8]) -> Result<(&[u8], &[u8]), Error> {
    let size = match bytes.first() {
        Some(2) => 4,    // Select one PCI device.
        Some(0x48) => 8, // Special interrupt source with an explicit handle.
        _ => return Err(Error::InvalidRecord),
    };
    let (entry, next) = bytes.split_at_checked(size).ok_or(Error::InvalidRecord)?;
    if entry[3] != 0 || (size == 8 && !matches!(entry[7], 1 | 2)) {
        return Err(Error::InvalidRecord);
    }
    Ok((entry, next))
}

fn decode(bytes: &[u8]) -> Result<Ivhd<'_>, Error> {
    let mut rest = bytes.get(48..).ok_or(Error::InvalidLength)?;
    let mut unit = None;
    let mut kinds = 0;
    while !rest.is_empty() {
        let head = rest.get(..4).ok_or(Error::InvalidRecord)?;
        let length = usize::from(u16::from_le_bytes(field(head, 2)));
        let (record, next) = rest.split_at_checked(length).ok_or(Error::InvalidRecord)?;
        let (header, kind) = match head[0] {
            0x10 => (24, 1),
            0x11 => (40, 2),
            // Memory reservations, exclusions, and other hardware scopes
            // require their own binding before this profile may be used.
            _ => return Err(Error::UnsupportedRevision),
        };
        let entries = record.get(header..).ok_or(Error::InvalidRecord)?;
        if entries.is_empty() {
            return Err(Error::InvalidRecord);
        }
        let mut remaining = entries;
        while !remaining.is_empty() {
            remaining = split_entry(remaining)?.1;
        }
        let address = u64::from_le_bytes(field(record, 8));
        if address == 0 || !address.is_multiple_of(16384) {
            return Err(Error::InvalidAddress);
        }
        let capability = u16::from_le_bytes(field(record, 6));
        if !(0x40..=0xfc).contains(&capability) || !capability.is_multiple_of(4) {
            return Err(Error::InvalidRecord);
        }
        let described = Ivhd {
            segment: u16::from_le_bytes(field(record, 16)),
            device: u16::from_le_bytes(field(record, 4)),
            capability,
            registers: PhysicalAddress::new(address)
                .range(ByteLength::new(16384))
                .ok_or(Error::InvalidAddress)?,
            entries,
        };
        if kind == 2 && u32::from_le_bytes(field(record, 20)) & 1 != 0 {
            return Err(Error::UnsupportedRevision);
        }
        if kinds & kind != 0 || unit.is_some_and(|prior| prior != described) {
            return Err(Error::DuplicateRecord);
        }
        kinds |= kind;
        unit = Some(described);
        rest = next;
    }
    unit.ok_or(Error::InvalidRecord)
}

#[cfg(test)]
#[path = "../../../../../tests/metal/platform/description/acpi/ivrs.rs"]
mod tests;

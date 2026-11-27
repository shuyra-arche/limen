//! VT-d remapping hardware described by a retained ACPI DMAR table.
//! A register extent permits discovery. Requester routing also needs the
//! original device scopes, PCI topology and reserved-memory constraints.

use super::{Error, Table, field};
use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

const HEADER: usize = 48;
const RECORD_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug)]
pub struct Dmar<'a>(Table<'a>);

/// A DRHD with its original device scope bytes and checked register extent.
#[derive(Clone, Copy, Debug)]
pub struct RemappingUnit<'a> {
    registers: PhysicalRange,
    segment: u16,
    include_pci_all: bool,
    scopes: &'a [u8],
    address_bits: u8,
}

impl<'a> RemappingUnit<'a> {
    pub fn registers(self) -> PhysicalRange {
        self.registers
    }

    pub fn segment(self) -> u16 {
        self.segment
    }

    pub fn includes_all_pci(self) -> bool {
        self.include_pci_all
    }

    pub fn host_address_bits(self) -> u8 {
        self.address_bits
    }

    pub fn device_scopes(self) -> &'a [u8] {
        self.scopes
    }

    /// Matches an explicitly listed endpoint on the starting bus. Bridge
    /// paths and segment-wide defaults require separate topology resolution.
    pub fn lists_root_endpoint(self, bus: u8, device: u8, function: u8) -> bool {
        let mut rest = self.scopes;
        while !rest.is_empty() {
            let (scope, next) = rest.split_at(usize::from(rest[1]));
            if scope[0] == 1 && scope[5] == bus && scope[6..] == [device, function] {
                return true;
            }
            rest = next;
        }
        false
    }

    fn decode(record: &'a [u8], address_bits: u8) -> Result<Self, Error> {
        if record.len() < 16 {
            return Err(Error::InvalidRecord);
        }
        let base = u64::from_le_bytes(field(record, 8));
        let bytes = 1u64
            .checked_shl(12 + u32::from(record[5]))
            .ok_or(Error::InvalidRecord)?;
        if base == 0 || !base.is_multiple_of(bytes) {
            return Err(Error::InvalidAddress);
        }
        let registers = PhysicalAddress::new(base)
            .range(ByteLength::new(bytes))
            .ok_or(Error::InvalidAddress)?;
        let scopes = &record[16..];
        let mut rest = scopes;
        while !rest.is_empty() {
            let length = usize::from(*rest.get(1).ok_or(Error::InvalidRecord)?);
            if length < 6 || !(length - 6).is_multiple_of(2) {
                return Err(Error::InvalidRecord);
            }
            let (scope, next) = rest.split_at_checked(length).ok_or(Error::InvalidRecord)?;
            if scope[6..]
                .as_chunks::<2>()
                .0
                .iter()
                .any(|path| path[0] > 31 || path[1] > 7)
            {
                return Err(Error::InvalidRecord);
            }
            rest = next;
        }
        Ok(Self {
            registers,
            segment: u16::from_le_bytes(field(record, 6)),
            include_pci_all: record[4] & 1 != 0,
            scopes,
            address_bits,
        })
    }
}

fn split(bytes: &[u8]) -> Result<(&[u8], &[u8]), Error> {
    let header = bytes.get(..4).ok_or(Error::InvalidRecord)?;
    let length = usize::from(u16::from_le_bytes(field(header, 2)));
    if length < 4 {
        return Err(Error::InvalidRecord);
    }
    bytes.split_at_checked(length).ok_or(Error::InvalidRecord)
}

impl<'a> Dmar<'a> {
    pub(in crate::metal::platform::description) fn validate(
        table: Table<'a>,
    ) -> Result<Self, Error> {
        let mut rest = table.bytes().get(HEADER..).ok_or(Error::InvalidLength)?;
        let address_bits = table.bytes()[36]
            .checked_add(1)
            .filter(|bits| (12..=64).contains(bits))
            .ok_or(Error::InvalidRecord)?;
        for _ in 0..RECORD_LIMIT {
            if rest.is_empty() {
                break;
            }
            let (record, next) = split(rest)?;
            if field::<2>(record, 0) == [0, 0] {
                RemappingUnit::decode(record, address_bits)?;
            }
            rest = next;
        }
        if !rest.is_empty() {
            return Err(Error::Capacity);
        }
        Ok(Self(table))
    }

    pub fn table(self) -> Table<'a> {
        self.0
    }

    pub fn units(self) -> impl Iterator<Item = RemappingUnit<'a>> {
        let mut rest = &self.0.bytes()[HEADER..];
        core::iter::from_fn(move || {
            while !rest.is_empty() {
                let (record, next) = split(rest).expect("validated DMAR record");
                rest = next;
                if field::<2>(record, 0) == [0, 0] {
                    return Some(
                        RemappingUnit::decode(record, self.0.bytes()[36] + 1)
                            .expect("validated DRHD"),
                    );
                }
            }
            None
        })
    }
}

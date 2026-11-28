//! Static HPET block identity and register geometry from the original ACPI table.

use super::{Error, Table, field};
use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

const LENGTH: usize = 56;
const REGISTER_BYTES: u64 = 1024;

/// An actual firmware-described HPET block, not a mapping or a counter.
#[derive(Clone, Copy, Debug)]
pub struct Hpet<'a> {
    table: Table<'a>,
    registers: PhysicalRange,
}

impl<'a> Hpet<'a> {
    pub(in crate::metal::platform::description) fn validate(
        table: Table<'a>,
    ) -> Result<Self, Error> {
        let bytes = table.bytes();
        if bytes.len() != LENGTH {
            return Err(Error::InvalidLength);
        }
        let id = u32::from_le_bytes(field(bytes, 36));
        let vendor = id >> 16;
        // HPET's GAS denotes the memory block. Some firmware leaves the
        // access width unspecified; accept that description, not arbitrary
        // address spaces, offsets or explicitly incompatible access widths.
        if id & 0xff == 0
            || id & (1 << 14) != 0
            || vendor == 0
            || vendor == 0xffff
            || bytes[40] != 0
            || !matches!(bytes[41], 0 | 64)
            || bytes[42] != 0
            || !matches!(bytes[43], 0 | 3 | 4)
            || bytes[55] & 3 == 3
        {
            return Err(Error::InvalidRecord);
        }
        let base = u64::from_le_bytes(field(bytes, 44));
        if base == 0 || !base.is_multiple_of(REGISTER_BYTES) {
            return Err(Error::InvalidAddress);
        }
        let registers = PhysicalAddress::new(base)
            .range(ByteLength::new(REGISTER_BYTES))
            .ok_or(Error::InvalidAddress)?;
        Ok(Self { table, registers })
    }

    pub const fn table(self) -> Table<'a> {
        self.table
    }

    pub const fn registers(self) -> PhysicalRange {
        self.registers
    }

    pub fn block_id(self) -> u32 {
        u32::from_le_bytes(field(self.table.bytes(), 36))
    }

    pub fn number(self) -> u8 {
        self.table.bytes()[52]
    }
}

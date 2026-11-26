use limen_treaty::model::PhysicalAddress;
use super::{Error, Table, field};

const HEADER: usize = 44;
const RECORD_LIMIT: usize = 256;
const AFFINITY_BITS: u64 = 0xff_00ff_ffff;
const ENABLED: u32 = 1;
const GICC_ONLINE_CAPABLE: u32 = 1 << 3;

/// Validated static interrupt-controller description from one ACPI table.
#[derive(Clone, Copy, Debug)]
pub struct Madt<'a> {
    table: Table<'a>,
    local_address: Option<PhysicalAddress>,
}

impl<'a> Madt<'a> {
    pub(super) fn new(table: Table<'a>) -> Result<Self, Error> {
        let bytes = table.bytes();
        let body = bytes.get(HEADER..).ok_or(Error::InvalidLength)?;
        let revision = table.revision();
        if !(1..=7).contains(&revision) {
            return Err(Error::UnsupportedRevision);
        }
        let mut rest = body;
        let mut offset = HEADER;
        let mut override_address = None;
        for _ in 0..RECORD_LIMIT {
            if rest.is_empty() {
                return Ok(Self::from_records(table, override_address));
            }
            let (record, next) = Record::split(rest, offset).ok_or(Error::InvalidRecord)?;
            if !record.valid(revision) {
                return Err(Error::InvalidRecord);
            }
            if records(&body[..offset - HEADER]).any(|prior| record.conflicts(prior)) {
                return Err(Error::DuplicateRecord);
            }
            if record.kind() == 5 {
                override_address = Some(u64::from_le_bytes(field(record.bytes, 4)));
            }
            offset += record.bytes.len();
            rest = next;
        }
        if !rest.is_empty() {
            return Err(Error::Capacity);
        }
        Ok(Self::from_records(table, override_address))
    }

    fn from_records(table: Table<'a>, override_address: Option<u64>) -> Self {
        let local_address = override_address
            .unwrap_or_else(|| u64::from(u32::from_le_bytes(field(table.bytes(), 36))));
        Self {
            table,
            local_address: (local_address != 0).then_some(PhysicalAddress::new(local_address)),
        }
    }

    pub const fn table(self) -> Table<'a> {
        self.table
    }

    /// Described local controller address, with a validated override applied.
    pub const fn local_controller_address(self) -> Option<PhysicalAddress> {
        self.local_address
    }

    pub fn records(self) -> impl Iterator<Item = Record<'a>> {
        records(&self.table.bytes()[HEADER..])
    }

    pub fn processors(self) -> impl Iterator<Item = Processor> + 'a {
        self.records().filter_map(Record::processor)
    }

    /// Whether the boot-enabled xAPIC topology includes this local APIC.
    pub fn enabled_local_apic(self, identity: u8) -> bool {
        self.processors().any(|processor| {
            matches!(processor, Processor::Apic { id, .. } if id == identity) && processor.enabled()
        })
    }

    pub fn io_apics(self) -> impl Iterator<Item = IoApic> + 'a {
        self.records().filter_map(Record::io_apic)
    }

    pub fn source_overrides(self) -> impl Iterator<Item = SourceOverride> + 'a {
        self.records().filter_map(Record::source_override)
    }
}

/// One bounded MADT record, identified by its byte offset in the table.
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    offset: usize,
    bytes: &'a [u8],
}

impl<'a> Record<'a> {
    fn split(bytes: &'a [u8], offset: usize) -> Option<(Self, &'a [u8])> {
        let length = usize::from(*bytes.get(1)?);
        if length < 2 {
            return None;
        }
        let (bytes, rest) = bytes.split_at_checked(length)?;
        Some((Self { offset, bytes }, rest))
    }

    pub const fn offset(self) -> usize {
        self.offset
    }

    pub const fn kind(self) -> u8 {
        self.bytes[0]
    }

    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    pub fn processor(self) -> Option<Processor> {
        let bytes = self.bytes;
        match self.kind() {
            0 => Some(Processor::Apic {
                id: bytes[3],
                uid: bytes[2],
                flags: u32::from_le_bytes(field(bytes, 4)),
            }),
            9 => Some(Processor::X2Apic {
                id: u32::from_le_bytes(field(bytes, 4)),
                uid: u32::from_le_bytes(field(bytes, 12)),
                flags: u32::from_le_bytes(field(bytes, 8)),
            }),
            11 => Some(Processor::Gicc {
                affinity: u64::from_le_bytes(field(bytes, 68)),
                uid: u32::from_le_bytes(field(bytes, 8)),
                flags: u32::from_le_bytes(field(bytes, 12)),
            }),
            _ => None,
        }
    }

    pub fn io_apic(self) -> Option<IoApic> {
        let bytes = self.bytes;
        (self.kind() == 1).then(|| IoApic {
            id: bytes[2],
            address: PhysicalAddress::new(u64::from(u32::from_le_bytes(field(bytes, 4)))),
            gsi_base: u32::from_le_bytes(field(bytes, 8)),
        })
    }

    pub fn source_override(self) -> Option<SourceOverride> {
        if self.kind() != 2 {
            return None;
        }
        let flags = u16::from_le_bytes(field(self.bytes, 8));
        let polarity = match flags & 3 {
            0 => Polarity::Conforming,
            1 => Polarity::ActiveHigh,
            3 => Polarity::ActiveLow,
            _ => return None,
        };
        let trigger = match (flags >> 2) & 3 {
            0 => Trigger::Conforming,
            1 => Trigger::Edge,
            3 => Trigger::Level,
            _ => return None,
        };
        Some(SourceOverride {
            isa_irq: self.bytes[3],
            gsi: u32::from_le_bytes(field(self.bytes, 4)),
            polarity,
            trigger,
        })
    }

    fn valid(self, revision: u8) -> bool {
        let bytes = self.bytes;
        match self.kind() {
            0 => bytes.len() == 8 && valid_apic_flags(u32::from_le_bytes(field(bytes, 4))),
            1 => bytes.len() == 12 && bytes[3] == 0 && u32::from_le_bytes(field(bytes, 4)) != 0,
            2 => {
                bytes.len() == 10
                    && bytes[2] == 0
                    && bytes[3] < 16
                    && u16::from_le_bytes(field(bytes, 8)) < 16
                    && self.source_override().is_some()
            }
            5 => {
                bytes.len() == 12
                    && field::<2>(bytes, 2) == [0; 2]
                    && u64::from_le_bytes(field(bytes, 4)) != 0
            }
            9 => {
                bytes.len() == 16
                    && field::<2>(bytes, 2) == [0; 2]
                    && valid_apic_flags(u32::from_le_bytes(field(bytes, 8)))
            }
            11 => {
                matches!(bytes.len(), 76 | 80 | 82)
                    && field::<2>(bytes, 2) == [0; 2]
                    && valid_gicc_flags(u32::from_le_bytes(field(bytes, 12)), revision)
                    && u64::from_le_bytes(field(bytes, 68)) & !AFFINITY_BITS == 0
            }
            _ => true,
        }
    }

    fn conflicts(self, other: Self) -> bool {
        match self.kind() {
            0 | 9 | 11 => self
                .processor()
                .zip(other.processor())
                .is_some_and(|(left, right)| {
                    left.present() && right.present() && left.same_identity(right)
                }),
            1 => self
                .io_apic()
                .zip(other.io_apic())
                .is_some_and(|(left, right)| {
                    left.id == right.id
                        || left.address == right.address
                        || left.gsi_base == right.gsi_base
                }),
            2 => self
                .source_override()
                .zip(other.source_override())
                .is_some_and(|(left, right)| left.isa_irq == right.isa_irq),
            5 => other.kind() == 5,
            _ => false,
        }
    }
}

fn records(mut bytes: &[u8]) -> impl Iterator<Item = Record<'_>> {
    let mut offset = HEADER;
    core::iter::from_fn(move || {
        let (record, rest) = Record::split(bytes, offset)?;
        offset += record.bytes.len();
        bytes = rest;
        Some(record)
    })
}

fn valid_apic_flags(flags: u32) -> bool {
    // Online Capable is reserved when Enabled is set.
    flags < 3
}

fn valid_gicc_flags(flags: u32, revision: u8) -> bool {
    // MADT revision 6 adds Online Capable and revision 7 adds GICR Non-coherent.
    let known = match revision {
        6 => 0b0_1111,
        7 => 0b1_1111,
        _ => 0b0_0111,
    };
    let enabled_and_online_capable = ENABLED | GICC_ONLINE_CAPABLE;
    flags & !known == 0 && flags & enabled_and_online_capable != enabled_and_online_capable
}

/// A described processor identity and its original flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Processor {
    Apic { id: u8, uid: u8, flags: u32 },
    X2Apic { id: u32, uid: u32, flags: u32 },
    Gicc { affinity: u64, uid: u32, flags: u32 },
}

impl Processor {
    pub const fn enabled(self) -> bool {
        match self {
            Self::Apic { flags, .. } | Self::X2Apic { flags, .. } | Self::Gicc { flags, .. } => {
                flags & ENABLED != 0
            }
        }
    }

    pub const fn online_capable(self) -> bool {
        if self.enabled() {
            return false;
        }
        match self {
            Self::Apic { flags, .. } | Self::X2Apic { flags, .. } => flags & 2 != 0,
            Self::Gicc { flags, .. } => flags & GICC_ONLINE_CAPABLE != 0,
        }
    }

    pub const fn apic_id(self) -> Option<u32> {
        match self {
            Self::Apic { id, .. } => Some(id as u32),
            Self::X2Apic { id, .. } => Some(id),
            Self::Gicc { .. } => None,
        }
    }

    fn present(self) -> bool {
        self.enabled() || self.online_capable()
    }

    fn uid(self) -> u32 {
        match self {
            Self::Apic { uid, .. } => u32::from(uid),
            Self::X2Apic { uid, .. } | Self::Gicc { uid, .. } => uid,
        }
    }

    fn same_identity(self, other: Self) -> bool {
        if self.uid() == other.uid() {
            return true;
        }
        match (self, other) {
            (
                Self::Gicc { affinity: left, .. },
                Self::Gicc {
                    affinity: right, ..
                },
            ) => left == right,
            _ => self
                .apic_id()
                .zip(other.apic_id())
                .is_some_and(|(left, right)| left == right),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoApic {
    pub id: u8,
    pub address: PhysicalAddress,
    /// The register-probed input count is needed to obtain the full GSI range.
    pub gsi_base: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceOverride {
    pub isa_irq: u8,
    pub gsi: u32,
    pub polarity: Polarity,
    pub trigger: Trigger,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Polarity {
    Conforming,
    ActiveHigh,
    ActiveLow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    Conforming,
    Edge,
    Level,
}

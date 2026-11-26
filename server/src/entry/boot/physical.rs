//! Physical exclusions and the conditions for retiring early use.

use super::{DtbMemory, FirmwareMap, Source};
use crate::address::{PhysicalAddress, PhysicalRange};
use core::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Occupant {
    Image,
    MajorStack,
    TrapStack,
    DeviceTree,
    FirmwareMap,
    Reservation(Source),
    Firmware { kind: u32, attributes: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Exclusion {
    pub physical: PhysicalRange,
    pub occupant: Occupant,
}

const RUNTIME: u64 = 1 << 63;
const CONVENTIONAL: u32 = 7;

impl Exclusion {
    /// Storage that must remain reserved throughout this handoff.
    pub fn fixed(self) -> bool {
        match self.occupant {
            // Loader and Boot Services allocations may be returned after all
            // remaining uses end. ACPI reclaim also needs ACPI enablement.
            Occupant::Firmware {
                kind: 1..=4,
                attributes,
            } => attributes & RUNTIME != 0,
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Condition {
    RuntimeStack = 1,
    ReadersGone = 2,
    TranslationRetired = 4,
    DevicesIsolated = 8,
}

/// Monotonic completion evidence for one physical scope. The scope alone
/// supplies no evidence.
pub struct Retirement {
    physical: PhysicalRange,
    observed: u8,
}

impl Retirement {
    pub fn new(physical: PhysicalRange) -> Self {
        Self {
            physical,
            observed: 0,
        }
    }

    /// Record one condition for this scope.
    ///
    /// # Safety
    ///
    /// The caller must have evidence from the responsible owners covering
    /// the entire scope. RuntimeStack means the permanent stack is active.
    /// ReadersGone includes other Harts and the last relevant reader.
    /// TranslationRetired covers old CPU translations and page-table walks.
    /// DevicesIsolated covers old DMA access, including device translations,
    /// page-table walks and in-flight transactions.
    /// The recorded condition must stay true through physical handoff.
    pub unsafe fn record(&mut self, condition: Condition) {
        self.observed |= condition as u8;
    }

    pub(super) fn completed(&self) -> Option<PhysicalRange> {
        const COMPLETE: u8 = Condition::RuntimeStack as u8
            | Condition::ReadersGone as u8
            | Condition::TranslationRetired as u8
            | Condition::DevicesIsolated as u8;
        (self.observed == COMPLETE).then_some(self.physical)
    }

    pub(crate) fn covers(&self, physical: PhysicalRange) -> bool {
        self.completed().is_some_and(|scope| scope.covers(physical))
    }
}

/// A bounded view of early occupations. A caller supplies retirement evidence
/// separately and retains the original descriptions through this view.
pub struct Exclusions<'a> {
    resident: [Option<Exclusion>; 5],
    map: Option<FirmwareMap<'a>>,
    memory: Option<&'a DtbMemory>,
}

impl<'a> Exclusions<'a> {
    pub(super) fn new(
        image: PhysicalRange,
        major: PhysicalRange,
        trap: PhysicalRange,
        tree: Option<Range<usize>>,
        map: Option<FirmwareMap<'a>>,
        memory: Option<&'a DtbMemory>,
    ) -> Self {
        let entry = |physical, occupant| Exclusion { physical, occupant };
        let map_storage = map.map(|map| {
            let bytes = map.descriptors().as_ptr_range();
            entry(
                extent(bytes.start.addr()..bytes.end.addr()),
                Occupant::FirmwareMap,
            )
        });
        Self {
            resident: [
                Some(entry(image, Occupant::Image)),
                Some(entry(major, Occupant::MajorStack)),
                Some(entry(trap, Occupant::TrapStack)),
                tree.map(|tree| entry(extent(tree), Occupant::DeviceTree)),
                map_storage,
            ],
            map,
            memory,
        }
    }

    /// Include every overlapping claim independently. The same bytes can
    /// have several occupants, such as a stack inside the resident image.
    pub fn entries(&self) -> impl Iterator<Item = Exclusion> + '_ {
        let reservations = self.memory.into_iter().flat_map(|memory| {
            memory.reservations().map(|reservation| Exclusion {
                physical: reservation.physical,
                occupant: Occupant::Reservation(reservation.source),
            })
        });
        let firmware = self.map.iter().flat_map(|map| {
            map.regions()
                .filter(|region| region.kind != CONVENTIONAL || region.attributes & RUNTIME != 0)
                .map(|region| Exclusion {
                    physical: region.physical,
                    occupant: Occupant::Firmware {
                        kind: region.kind,
                        attributes: region.attributes,
                    },
                })
        });
        self.resident
            .iter()
            .flatten()
            .copied()
            .chain(reservations)
            .chain(firmware)
    }

    /// Test a candidate physical range against the current exclusions. Any
    /// overlap retains the whole queried range. The caller still validates RAM
    /// provenance, granularity and access before handing it to an allocator.
    pub fn retains(&self, physical: PhysicalRange, retirement: Option<&Retirement>) -> bool {
        if !retirement.is_some_and(|evidence| evidence.covers(physical)) {
            return true;
        }
        self.entries()
            .any(|entry| entry.fixed() && entry.physical.overlaps(physical))
    }
}

pub(super) fn extent(range: Range<usize>) -> PhysicalRange {
    PhysicalRange::new(
        PhysicalAddress::new(range.start as u64),
        PhysicalAddress::new(range.end as u64),
    )
    .expect("nonempty extent supplied by the boot owner")
}

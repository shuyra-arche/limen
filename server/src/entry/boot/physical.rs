//! Physical exclusions and the conditions for retiring early use.

use super::acpi::Acpi;
use super::{DtbMemory, FirmwareMap, Source};
use crate::metal::{
    memory::allocator::treaty::{Allocator, Error, Grant, RegionDescription},
    platform::firmware::treaty::map::MemoryUse,
};
use limen_treaty::model::PhysicalRange;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Occupant {
    Image,
    MajorStack,
    TrapStack,
    DeviceTree,
    Acpi,
    FirmwareMap,
    Reservation(Source),
    Firmware(MemoryUse),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Exclusion {
    pub physical: PhysicalRange,
    pub occupant: Occupant,
}

impl Exclusion {
    /// Storage that must remain reserved throughout this handoff.
    pub fn fixed(self) -> bool {
        self.occupant != Occupant::Firmware(MemoryUse::Reclaimable)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Condition {
    RuntimeStack = 1,
    ReadersGone = 2,
    TranslationRetired = 4,
    DevicesIsolated = 8,
    NoEarlyBorrow = 16,
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
    /// ReadersGone includes other processors and the last relevant reader.
    /// NoEarlyBorrow means the extent is independent of all such stack
    /// and reader lifetimes. Absence from firmware reservations alone
    /// cannot prove this. TranslationRetired covers old CPU translations
    /// and page-table walks. DevicesIsolated covers old device translations,
    /// walks and in-flight transactions. For those hardware conditions the
    /// original owner may prove that a path never had access to this extent.
    /// The recorded condition must stay true through physical handoff.
    pub unsafe fn record(&mut self, condition: Condition) {
        self.observed |= condition as u8;
    }

    pub(super) fn completed(&self) -> Option<PhysicalRange> {
        let hardware_safe =
            self.has(Condition::TranslationRetired) && self.has(Condition::DevicesIsolated);
        let readers_safe = self.closed_early_borrows() || self.has(Condition::NoEarlyBorrow);
        (hardware_safe && readers_safe).then_some(self.physical)
    }

    fn has(&self, condition: Condition) -> bool {
        self.observed & condition as u8 != 0
    }

    fn closed_early_borrows(&self) -> bool {
        self.has(Condition::RuntimeStack) && self.has(Condition::ReadersGone)
    }

    /// Prepare the original owner's delayed admission handoff.
    ///
    /// # Safety
    ///
    /// The caller must still have exclusive custody of this ordinary RAM,
    /// must be entitled to hand it over to the given source, and must
    /// exclude all fixed and already admitted RAM. The retirement evidence
    /// and unique handoff must both remain valid through admission.
    pub unsafe fn grant(
        &self,
        source: &'static Allocator,
        description: RegionDescription,
    ) -> Result<Grant, Error> {
        if !self.covers(description.physical) {
            return Err(Error::Range);
        }
        unsafe { Grant::from_retired(source, description) }
    }

    fn covers(&self, physical: PhysicalRange) -> bool {
        self.completed().is_some_and(|scope| scope.covers(physical))
    }
}

/// A bounded view of early occupations. A caller supplies retirement evidence
/// separately and retains the original descriptions through this view.
pub struct Exclusions<'a> {
    resident: [Option<Exclusion>; 5],
    map: Option<FirmwareMap<'a>>,
    memory: Option<&'a DtbMemory>,
    acpi: Option<&'a Acpi<'a>>,
}

impl<'a> Exclusions<'a> {
    #[cfg_attr(
        not(any(limen_target, test)),
        expect(
            dead_code,
            reason = "native entry supplies the original occupied ranges"
        )
    )]
    pub(super) fn new(
        image: PhysicalRange,
        major: PhysicalRange,
        trap: PhysicalRange,
        tree: Option<PhysicalRange>,
        map: Option<FirmwareMap<'a>>,
        map_storage: Option<PhysicalRange>,
        memory: Option<&'a DtbMemory>,
    ) -> Self {
        let entry = |physical, occupant| Exclusion { physical, occupant };
        Self {
            resident: [
                Some(entry(image, Occupant::Image)),
                Some(entry(major, Occupant::MajorStack)),
                Some(entry(trap, Occupant::TrapStack)),
                tree.map(|physical| entry(physical, Occupant::DeviceTree)),
                map_storage.map(|physical| entry(physical, Occupant::FirmwareMap)),
            ],
            map,
            memory,
            acpi: None,
        }
    }

    #[cfg_attr(
        not(any(limen_target, test)),
        expect(
            dead_code,
            reason = "native entry supplies the retained ACPI description"
        )
    )]
    pub(super) fn with_acpi(mut self, acpi: Option<&'a Acpi<'a>>) -> Self {
        self.acpi = acpi;
        self
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
            map.regions().filter_map(|region| {
                let usage = region.memory_use();
                (usage != MemoryUse::Conventional).then_some(Exclusion {
                    physical: region.physical,
                    occupant: Occupant::Firmware(usage),
                })
            })
        });
        self.resident
            .iter()
            .flatten()
            .copied()
            .chain(reservations)
            .chain(firmware)
            .chain(self.acpi.into_iter().flat_map(|acpi| {
                core::iter::once(acpi.rsdp_range())
                    .chain(core::iter::once(acpi.xsdt().physical()))
                    .chain(acpi.tables().map(|table| table.physical()))
                    .map(|physical| Exclusion {
                        physical,
                        occupant: Occupant::Acpi,
                    })
            }))
    }

    /// Test a candidate physical range against the current exclusions. Any
    /// overlap retains the whole queried range. The caller still validates RAM
    /// provenance, granularity and access before handing it to an allocator.
    pub fn retains(&self, physical: PhysicalRange, retirement: Option<&Retirement>) -> bool {
        if !retirement.is_some_and(|evidence| evidence.covers(physical)) {
            return true;
        }
        let closed_early_borrows = retirement.is_some_and(Retirement::closed_early_borrows);
        self.entries().any(|entry| {
            entry.physical.overlaps(physical) && (entry.fixed() || !closed_early_borrows)
        })
    }
}

//! Organize early physical ranges and reserve explicitly mapped root storage.

use super::{Boot, Exclusions, Retirement};
use crate::metal::{
    memory::access::treaty::ram::MappedRange,
    memory::allocator::treaty::{
        Allocator, Allowance, Config, Error, Initial, InitialRegions, RegionDescription,
        RegionOrigin, RootInitialization, RootReservation, RootStorage,
    },
    platform::{
        description::treaty::devicetree::memory::DtbMemory,
        firmware::treaty::map::{FirmwareMap, FirmwareRegion, PAGE_ALIGNMENT},
    },
};
use core::{alloc::Layout, iter};
use limen_treaty::model::{Alignment, ByteLength, PhysicalRange};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionError {
    Granule,
    Range,
    Overlap,
    Capacity,
    Layout,
}

/// Checked geometry in borrowed scratch storage. The ranges describe physical
/// candidates for the next handoff and grant no addressability or ownership.
pub struct Regions<'a> {
    entries: &'a mut [Option<RegionDescription>],
    length: usize,
    granule: Alignment,
}

struct RootPlan {
    index: usize,
    region: RegionDescription,
    reserved: PhysicalRange,
}

impl RootPlan {
    fn remainder(&self) -> [Option<RegionDescription>; 2] {
        let physical = self.region.physical;
        let part = |start, end| {
            PhysicalRange::new(start, end).map(|physical| RegionDescription {
                physical,
                origin: self.region.origin,
            })
        };
        [
            part(physical.start(), self.reserved.start()),
            part(self.reserved.end(), physical.end()),
        ]
    }
}

impl<'a> Regions<'a> {
    #[cfg_attr(
        not(any(limen_target, test)),
        expect(dead_code, reason = "native entry supplies the original exclusions")
    )]
    pub(crate) fn organize(
        boot: &Boot<'_>,
        exclusions: &Exclusions<'_>,
        granule: ByteLength,
        retirement: Option<&Retirement>,
        storage: &'a mut [Option<RegionDescription>],
    ) -> Result<Self, RegionError> {
        storage.fill(None);
        let granule = Alignment::new(granule.get()).ok_or(RegionError::Granule)?;
        let mut regions = Self {
            entries: storage,
            length: 0,
            granule,
        };
        let Some(scope) = retirement.and_then(Retirement::completed) else {
            return Ok(regions);
        };
        if let Err(error) = regions.collect(boot, exclusions, scope) {
            regions.entries.fill(None);
            return Err(error);
        }
        Ok(regions)
    }

    pub fn entries(&self) -> impl Iterator<Item = RegionDescription> + '_ {
        self.entries[..self.length].iter().flatten().copied()
    }

    fn collect(
        &mut self,
        boot: &Boot<'_>,
        exclusions: &Exclusions<'_>,
        scope: PhysicalRange,
    ) -> Result<(), RegionError> {
        if let Some(regions) = boot.conventional_ram() {
            for region in regions {
                self.exclude(
                    region.physical,
                    RegionOrigin::DeviceTreeAndUefi {
                        attributes: region.attributes,
                    },
                    exclusions,
                    scope,
                )?;
            }
        } else if let Some(map) = boot.firmware_map() {
            for region in map.conventional() {
                self.exclude(
                    region.physical,
                    RegionOrigin::Uefi {
                        attributes: region.attributes,
                    },
                    exclusions,
                    scope,
                )?;
            }
        } else if let Some(memory) = boot.dtb_memory() {
            for region in memory.ram() {
                self.exclude(region, RegionOrigin::DeviceTree, exclusions, scope)?;
            }
        }
        Ok(())
    }

    fn exclude(
        &mut self,
        candidate: PhysicalRange,
        origin: RegionOrigin,
        exclusions: &Exclusions<'_>,
        scope: PhysicalRange,
    ) -> Result<(), RegionError> {
        let Some(candidate) = candidate.intersect(scope) else {
            return Ok(());
        };
        let end = candidate.end().align_down(self.granule);
        let start = candidate.start();
        if start >= end {
            return Ok(());
        }
        // Since end is aligned, rounding an address below it cannot overflow.
        let mut cursor = start.align_up(self.granule).ok_or(RegionError::Range)?;
        while cursor < end {
            let mut next = end;
            let mut blocked = cursor;
            for claim in exclusions.entries().filter(|entry| entry.fixed()) {
                let first = claim.physical.start().align_down(self.granule);
                if first > cursor {
                    next = next.min(first);
                } else {
                    let last = claim.physical.end().align_up(self.granule).unwrap_or(end);
                    blocked = blocked.max(last.min(end));
                }
            }
            if blocked > cursor {
                cursor = blocked;
            } else {
                self.insert(RegionDescription {
                    physical: PhysicalRange::new(cursor, next).expect("nonempty candidate"),
                    origin,
                })?;
                cursor = next;
            }
        }
        Ok(())
    }

    fn insert(&mut self, region: RegionDescription) -> Result<(), RegionError> {
        let position = self.entries[..self.length].partition_point(|entry| {
            entry.expect("occupied region").physical.start() < region.physical.start()
        });
        if (position > 0
            && self.entries[position - 1]
                .unwrap()
                .physical
                .overlaps(region.physical))
            || (position < self.length
                && region
                    .physical
                    .overlaps(self.entries[position].unwrap().physical))
        {
            return Err(RegionError::Overlap);
        }
        if self.length == self.entries.len() {
            return Err(RegionError::Capacity);
        }
        self.entries
            .copy_within(position..self.length, position + 1);
        self.entries[position] = Some(region);
        self.length += 1;
        Ok(())
    }

    /// Reserve one layout from a real mapped grant. With no qualifying grant,
    /// return no root and leave the geometry and input unchanged. Layout and
    /// range capacity checks precede consumption or mutation.
    pub fn reserve_root<'m>(
        &mut self,
        mapping: &mut Option<MappedRange<'m>>,
        layout: Layout,
    ) -> Result<Option<RootStorage<'m>>, RegionError> {
        let Some((plan, root)) = self.plan_root(mapping, layout)? else {
            return Ok(None);
        };
        let storage = root.reserve();
        self.remove_root(plan);
        Ok(Some(storage))
    }

    /// Establish the fixed root and deliver initial Issued allocations before
    /// publishing the source. Deposit the initial Allowance's close handle
    /// in its owner's receiver before publication as well. Rejection leaves
    /// the mapping, ranges and both receivers with the caller. Absent qualified
    /// mapping yields no source.
    ///
    /// # Safety
    ///
    /// This is the first handoff of the described RAM by its exclusive boot
    /// owner. The RAM must remain isolated, excluded from other sources and
    /// uncommitted except for the listed initial receivers. The mapping must
    /// be canonical permanent root storage. Receiver slots belong to their
    /// actual initial consumers and must remain recoverable after handoff.
    pub unsafe fn allocator(
        &mut self,
        mapping: &mut Option<MappedRange<'static>>,
        config: Config,
        capacity: u64,
        allowance_receiver: &mut Option<Allowance>,
        initial: &mut [Initial<'_>],
    ) -> Result<Option<&'static Allocator>, Error> {
        if config.granule != ByteLength::new(self.granule.get()) {
            return Err(Error::Layout);
        }
        let Some((plan, root)) = self
            .plan_root(mapping, config.layout()?)
            .map_err(root_error)?
        else {
            return Ok(None);
        };
        let prepared = RootInitialization::prepare(
            config,
            root,
            self.after_root(&plan),
            capacity,
            allowance_receiver,
            initial,
        )?;
        let source = unsafe { prepared.install() };
        self.clear();
        Ok(Some(source))
    }

    fn after_root(&self, plan: &RootPlan) -> InitialRegions<'_> {
        InitialRegions {
            before: &self.entries[..plan.index],
            remainder: plan.remainder(),
            after: &self.entries[plan.index + 1..self.length],
        }
    }

    fn clear(&mut self) {
        self.entries[..self.length].fill(None);
        self.length = 0;
    }

    fn plan_root<'r, 'm>(
        &self,
        mapping: &'r mut Option<MappedRange<'m>>,
        layout: Layout,
    ) -> Result<Option<(RootPlan, RootReservation<'r, 'm>)>, RegionError> {
        let Some(mapped) = mapping.as_ref() else {
            return Ok(None);
        };
        let Some((index, region)) = self
            .entries()
            .enumerate()
            .find(|(_, region)| region.physical.covers(mapped.physical()))
        else {
            return Ok(None);
        };
        let Some(root) =
            RootReservation::prepare(mapping, layout, self.granule, region).map_err(|error| {
                match error {
                    Error::Range => RegionError::Range,
                    _ => RegionError::Layout,
                }
            })?
        else {
            return Ok(None);
        };
        let reserved = root.reserved();
        let left = region.physical.start() < reserved.start();
        let right = reserved.end() < region.physical.end();
        if left && right && self.length == self.entries.len() {
            return Err(RegionError::Capacity);
        }
        Ok(Some((
            RootPlan {
                index,
                region,
                reserved,
            },
            root,
        )))
    }

    fn remove_root(&mut self, plan: RootPlan) {
        let [left, right] = plan.remainder();
        let index = plan.index;
        match (left, right) {
            (Some(left), Some(right)) => {
                self.entries.copy_within(index + 1..self.length, index + 2);
                self.length += 1;
                self.entries[index] = Some(left);
                self.entries[index + 1] = Some(right);
            }
            (Some(remainder), None) | (None, Some(remainder)) => {
                self.entries[index] = Some(remainder);
            }
            (None, None) => {
                self.entries.copy_within(index + 1..self.length, index);
                self.length -= 1;
                self.entries[self.length] = None;
            }
        }
    }
}

fn root_error(reason: RegionError) -> Error {
    match reason {
        RegionError::Capacity => Error::Capacity,
        RegionError::Overlap => Error::Overlap,
        RegionError::Range => Error::Range,
        RegionError::Granule | RegionError::Layout => Error::Layout,
    }
}

/// Cross-description reconciliation. The original descriptors retain their
/// classification; this yields only candidate ranges, never allocation rights.
pub(super) fn conventional_ram<'a>(
    map: &'a FirmwareMap<'_>,
    memory: &'a DtbMemory,
) -> impl Iterator<Item = FirmwareRegion> + 'a {
    map.conventional().flat_map(move |region| {
        memory
            .ram()
            .filter_map(move |bank| {
                // Keep only complete UEFI pages supported by the DTB.
                complete_pages(region.physical.intersect(bank)?)
            })
            .flat_map(move |pages| unreserved(pages, memory))
            .map(move |physical| FirmwareRegion {
                kind: region.kind,
                physical,
                attributes: region.attributes,
            })
    })
}

fn complete_pages(range: PhysicalRange) -> Option<PhysicalRange> {
    let start = range.start().align_up(PAGE_ALIGNMENT)?;
    let end = range.end().align_down(PAGE_ALIGNMENT);
    PhysicalRange::new(start, end)
}

/// Walk one intersection using a cursor over a bounded reservation list.
/// All reservations, including overlapping ones, cover any page they touch.
fn unreserved(
    range: PhysicalRange,
    memory: &DtbMemory,
) -> impl Iterator<Item = PhysicalRange> + '_ {
    let mut start = range.start();
    let end = range.end();
    iter::from_fn(move || {
        while start < end {
            let mut next = end;
            let mut blocked = start;
            for reservation in memory.reservations() {
                let base = reservation.physical.start().align_down(PAGE_ALIGNMENT);
                if base > start {
                    next = next.min(base);
                } else {
                    let last = reservation.physical.end().align_up(PAGE_ALIGNMENT);
                    blocked = blocked.max(last.unwrap_or(end).min(end));
                }
            }
            if blocked > start {
                start = blocked;
            } else {
                let segment = PhysicalRange::new(start, next).expect("nonempty unreserved segment");
                start = next;
                return Some(segment);
            }
        }
        None
    })
}

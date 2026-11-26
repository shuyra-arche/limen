//! Organize early physical ranges and reserve explicitly mapped root storage.

use super::{Boot, Exclusions, Retirement, bump::Bump};
use crate::{access::MappedRange, address::PhysicalRange};
use core::{alloc::Layout, mem::MaybeUninit};
use limen_treaty::model::{Alignment, ByteLength};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionError {
    Granule,
    Range,
    Overlap,
    Capacity,
    Layout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionSource {
    DeviceTree,
    Uefi { attributes: u64 },
    DeviceTreeAndUefi { attributes: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Region {
    pub physical: PhysicalRange,
    pub source: RegionSource,
}

/// Checked geometry in borrowed scratch storage. The ranges describe physical
/// candidates for the next handoff and grant no addressability or ownership.
pub struct Regions<'a> {
    entries: &'a mut [Option<Region>],
    length: usize,
    granule: Alignment,
}

/// Storage reserved for a root layout. Its entire backing granules have
/// already been removed from the geometry.
pub struct RootStorage<'a> {
    mapping: MappedRange<'a>,
    reserved: PhysicalRange,
    source: RegionSource,
}

impl<'a> RootStorage<'a> {
    pub fn physical(&self) -> PhysicalRange {
        self.mapping.physical()
    }

    pub fn reserved(&self) -> PhysicalRange {
        self.reserved
    }

    pub fn source(&self) -> RegionSource {
        self.source
    }

    pub(crate) fn into_bytes(self) -> &'a mut [MaybeUninit<u8>] {
        self.mapping.into_bytes()
    }

    pub fn bytes(&mut self) -> &mut [MaybeUninit<u8>] {
        self.mapping.bytes()
    }
}

pub(crate) struct RootPlan {
    index: usize,
    region: Region,
    physical: PhysicalRange,
    reserved: PhysicalRange,
    offset: core::ops::Range<usize>,
}

impl RootPlan {
    fn remainder(&self) -> [Option<Region>; 2] {
        let physical = self.region.physical;
        let part = |start, end| {
            PhysicalRange::new(start, end).map(|physical| Region {
                physical,
                source: self.region.source,
            })
        };
        [
            part(physical.start(), self.reserved.start()),
            part(self.reserved.end(), physical.end()),
        ]
    }
}

impl<'a> Regions<'a> {
    pub(super) fn organize(
        boot: &Boot<'_>,
        exclusions: &Exclusions<'_>,
        granule: u64,
        retirement: Option<&Retirement>,
        storage: &'a mut [Option<Region>],
    ) -> Result<Self, RegionError> {
        storage.fill(None);
        let granule = Alignment::new(granule).ok_or(RegionError::Granule)?;
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

    pub fn entries(&self) -> impl Iterator<Item = Region> + '_ {
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
                    RegionSource::DeviceTreeAndUefi {
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
                    RegionSource::Uefi {
                        attributes: region.attributes,
                    },
                    exclusions,
                    scope,
                )?;
            }
        } else if let Some(memory) = boot.dtb_memory() {
            for region in memory.ram() {
                self.exclude(region, RegionSource::DeviceTree, exclusions, scope)?;
            }
        }
        Ok(())
    }

    fn exclude(
        &mut self,
        candidate: PhysicalRange,
        source: RegionSource,
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
                self.insert(Region {
                    physical: PhysicalRange::new(cursor, next).expect("nonempty candidate"),
                    source,
                })?;
                cursor = next;
            }
        }
        Ok(())
    }

    fn insert(&mut self, region: Region) -> Result<(), RegionError> {
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
        Ok(self
            .plan_root(mapping.as_ref(), layout)?
            .map(|plan| self.take_root(mapping, plan)))
    }

    pub(crate) fn granule(&self) -> u64 {
        self.granule.get()
    }

    pub(crate) fn after_root<'r>(
        &'r self,
        plan: &'r RootPlan,
    ) -> impl Iterator<Item = Region> + 'r {
        self.entries[..plan.index]
            .iter()
            .flatten()
            .copied()
            .chain(plan.remainder().into_iter().flatten())
            .chain(
                self.entries[plan.index + 1..self.length]
                    .iter()
                    .flatten()
                    .copied(),
            )
    }

    pub(crate) fn clear(&mut self) {
        self.entries[..self.length].fill(None);
        self.length = 0;
    }

    pub(crate) fn plan_root(
        &self,
        mapping: Option<&MappedRange<'_>>,
        layout: Layout,
    ) -> Result<Option<RootPlan>, RegionError> {
        let Some(mapped) = mapping else {
            return Ok(None);
        };
        let Some((index, region)) = self
            .entries()
            .enumerate()
            .find(|(_, region)| region.physical.covers(mapped.physical()))
        else {
            return Ok(None);
        };
        let address = mapped.address();
        let limit = address
            .checked_add(mapped.length())
            .ok_or(RegionError::Range)?;
        if layout.size() == 0
            || mapped.physical().start().get() % layout.align() as u64
                != (address % layout.align()) as u64
        {
            return Err(RegionError::Layout);
        }
        let mut bump = Bump::new(address..limit).ok_or(RegionError::Layout)?;
        let bytes = bump.reserve(layout).ok_or(RegionError::Layout)?;
        let offset = bytes.start - address..bytes.end - address;
        // Bump proved the offsets within the slice, whose length matches
        // the checked physical extent.
        let physical = mapped
            .physical()
            .start()
            .advance(ByteLength::new(offset.start as u64))
            .and_then(|start| start.span(ByteLength::new(offset.len() as u64)))
            .ok_or(RegionError::Range)?;
        let reserved = PhysicalRange::new(
            physical.start().align_down(self.granule),
            physical
                .end()
                .align_up(self.granule)
                .ok_or(RegionError::Range)?,
        )
        .ok_or(RegionError::Range)?;
        let left = region.physical.start() < reserved.start();
        let right = reserved.end() < region.physical.end();
        if left && right && self.length == self.entries.len() {
            return Err(RegionError::Capacity);
        }

        Ok(Some(RootPlan {
            index,
            region,
            physical,
            reserved,
            offset,
        }))
    }

    pub(crate) fn take_root<'m>(
        &mut self,
        mapping: &mut Option<MappedRange<'m>>,
        plan: RootPlan,
    ) -> RootStorage<'m> {
        let [left, right] = plan.remainder();
        let RootPlan {
            index,
            region,
            physical,
            reserved,
            offset,
        } = plan;
        let mut storage = mapping.take().expect("checked mapping");
        let length = offset.end - offset.start;
        if offset.start != 0 {
            let (_, suffix) = storage
                .split(offset.start)
                .ok()
                .expect("checked root start");
            storage = suffix;
        }
        if length != storage.length() {
            let (prefix, _) = storage.split(length).ok().expect("checked root end");
            storage = prefix;
        }
        assert_eq!(storage.physical(), physical);
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
        RootStorage {
            mapping: storage,
            reserved,
            source: region.source,
        }
    }
}

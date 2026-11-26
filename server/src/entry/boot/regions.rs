//! Organize early physical ranges and reserve explicitly mapped root storage.

use super::{Boot, Exclusions, Retirement, bump::Bump};
use crate::{
    access::MappedRange,
    address::{PhysicalRange, length},
    allocator::{
        Allocator, Allowance, Config, Error, Initial, RegionDescription, RegionOrigin,
        RootInitialization, RootStorage,
    },
};
use core::alloc::Layout;
use limen_treaty::model::{Alignment, ByteLength};

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
    physical: PhysicalRange,
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
    pub(super) fn organize(
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
        Ok(self
            .plan_root(mapping.as_ref(), layout)?
            .map(|plan| self.take_root(mapping, plan)))
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
        let Some(plan) = self
            .plan_root(mapping.as_ref(), config.layout()?)
            .map_err(root_error)?
        else {
            return Ok(None);
        };
        let prepared = RootInitialization::prepare(
            config,
            self.after_root(&plan),
            capacity,
            allowance_receiver,
            initial,
        )?;
        let storage = self.take_root(mapping, plan);
        // The same root plan chose this mapping with config.layout() and
        // removed its whole reservation from the prepared region list.
        let source = unsafe { prepared.install(storage) };
        self.clear();
        Ok(Some(source))
    }

    fn after_root<'r>(
        &'r self,
        plan: &'r RootPlan,
    ) -> impl Iterator<Item = RegionDescription> + 'r {
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

    fn clear(&mut self) {
        self.entries[..self.length].fill(None);
        self.length = 0;
    }

    fn plan_root(
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
        let offset = u64::try_from(bytes.start - address).map_err(|_| RegionError::Range)?;
        let length = u64::try_from(bytes.len()).map_err(|_| RegionError::Range)?;
        // Bump proved the offsets within the slice, whose length matches
        // the checked physical extent.
        let physical = mapped
            .physical()
            .start()
            .advance(ByteLength::new(offset))
            .and_then(|start| start.range(ByteLength::new(length)))
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
        }))
    }

    fn take_root<'m>(
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
        } = plan;
        let mut storage = mapping.take().expect("checked mapping");
        let start = storage.physical().start();
        if physical.start() != start {
            let offset = physical
                .start()
                .distance(start)
                .expect("checked root offset");
            let (_, suffix) = storage.split(offset).ok().expect("checked root start");
            storage = suffix;
        }
        if physical.end() != storage.physical().end() {
            let (prefix, _) = storage
                .split(length(physical))
                .ok()
                .expect("checked root end");
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
        RootStorage::new(storage, reserved, region.origin)
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

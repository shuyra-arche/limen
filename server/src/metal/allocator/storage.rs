//! Prepay the stable root before subtracting its physical backing from RAM.
use core::{
    alloc::Layout,
    mem::MaybeUninit,
    ptr, slice,
    sync::atomic::{AtomicUsize, Ordering::Release},
};
use limen_treaty::model::{Alignment, ByteLength};

use crate::{
    access::MappedRange,
    address::{PhysicalRange, length},
    entry::boot::{RegionError, Regions, RootStorage},
    metal::identity::Entry,
    sync::TurnLock,
};

use super::{
    Allocator, Description, Error, REGION_LIMIT, Region, RegionId,
    allowance::{Allowance, Balance, Free, Slot, SlotState},
    buddy::{Geometry, Root},
    bump::Bump,
    region::Backing,
    transfer::{Allocation, AllocationId, AllocationRecord, Phase, ReturnRecord},
};

/// Per-position maximum capacities include unpublished Region positions.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub granule: ByteLength,
    pub maximum: ByteLength,
    pub regions: usize,
    pub grains: usize,
    pub roots: usize,
    pub allocations: usize,
    pub returns: usize,
    pub allowances: usize,
    pub edits: usize,
}

impl Config {
    fn check(self) -> Result<(), Error> {
        let alignment = Alignment::new(self.granule.get()).ok_or(Error::Layout)?;
        if !alignment.is_aligned(self.maximum) {
            return Err(Error::Layout);
        }
        if !(1..=REGION_LIMIT).contains(&self.regions)
            || self.roots == 0
            || self.grains == 0
            || self.allocations == 0
            || self.returns < self.allocations
            || self.allowances == 0
            || u32::try_from(self.allocations - 1).is_err()
            || u32::try_from(self.returns - 1).is_err()
        {
            return Err(Error::Capacity);
        }
        Ok(())
    }

    pub(super) fn granule_alignment(self) -> Alignment {
        Alignment::new(self.granule.get()).expect("validated Allocator configuration")
    }

    fn nodes(self) -> Result<usize, Error> {
        self.grains.checked_mul(2).ok_or(Error::Capacity)
    }

    /// Full fixed root, including backing for delayed admission and history.
    pub fn layout(self) -> Result<Layout, Error> {
        self.check()?;
        let mut root = Layout::new::<Allocator>();
        for element in [
            array::<Region>(self.regions)?,
            array::<TurnLock<Slot>>(self.allowances)?,
        ] {
            root = root.extend(element).map_err(|_| Error::Layout)?.0;
        }
        let region = [
            array::<Root>(self.roots)?,
            array::<u8>(self.nodes()?)?,
            array::<Entry<AllocationRecord>>(self.allocations)?,
            array::<Entry<ReturnRecord>>(self.returns)?,
        ];
        for _ in 0..self.regions {
            for element in region {
                root = root.extend(element).map_err(|_| Error::Layout)?.0;
            }
        }
        Ok(root.pad_to_align())
    }

    pub(super) fn geometry(self, description: Description) -> Result<Geometry, Error> {
        let geometry =
            Geometry::new(description.physical, self.granule).map_err(|_| Error::Range)?;
        if length(description.physical).get() / self.granule.get() > self.grains as u64
            || geometry.root_count() > self.roots
            || geometry.edit_bound() > self.edits
        {
            return Err(Error::Capacity);
        }
        Ok(geometry)
    }
}

fn array<T>(count: usize) -> Result<Layout, Error> {
    Layout::array::<T>(count).map_err(|_| Error::Layout)
}

/// The unique original Allocation paired with its exact exclusive mapping.
/// Neither a physical address nor a same-length slice can substitute for
/// the original mapped material.
#[must_use]
pub struct MappedStorage<'a> {
    allocation: Allocation,
    mapping: MappedRange<'a>,
}

impl<'a> MappedStorage<'a> {
    /// # Safety
    /// Both inputs must be valid independently. If their ranges match, the
    /// caller must establish that the mapping belongs to this exact issued
    /// Allocation, not a different generation at the same address. The
    /// original owner must preserve the actual mapping and exclusive-storage
    /// obligations through all derived uses and their settlement. Range
    /// equality proves geometry only, not identity or hardware lifetime.
    pub unsafe fn prepare(
        allocation: Allocation,
        mapping: MappedRange<'a>,
    ) -> Result<Self, (Allocation, MappedRange<'a>, Error)> {
        if allocation.physical() != mapping.physical() {
            return Err((allocation, mapping, Error::Range));
        }
        Ok(Self {
            allocation,
            mapping,
        })
    }

    pub fn id(&self) -> AllocationId {
        self.allocation.id()
    }

    pub fn physical(&self) -> PhysicalRange {
        self.mapping.physical()
    }

    /// Recover the two original inputs from a private preparation. This
    /// does not retire the mapping, the Allocation, or any hardware effect.
    pub fn into_parts(self) -> (Allocation, MappedRange<'a>) {
        (self.allocation, self.mapping)
    }

    pub(super) fn allocation(&self) -> &Allocation {
        &self.allocation
    }

    pub(super) fn mapping(&self) -> &MappedRange<'a> {
        &self.mapping
    }

    pub(super) fn mapping_mut(&mut self) -> &mut MappedRange<'a> {
        &mut self.mapping
    }
}

/// One actual initial allocation and its original receiver slot.
pub struct Initial<'a> {
    physical: PhysicalRange,
    receiver: &'a mut Option<Allocation>,
    staged: Option<AllocationId>,
}

impl<'a> Initial<'a> {
    pub fn new(physical: PhysicalRange, receiver: &'a mut Option<Allocation>) -> Self {
        Self {
            physical,
            receiver,
            staged: None,
        }
    }
}

impl Regions<'_> {
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
        if config.granule != self.granule() {
            return Err(Error::Layout);
        }
        let Some(plan) = self
            .plan_root(mapping.as_ref(), config.layout()?)
            .map_err(root_error)?
        else {
            return Ok(None);
        };
        if allowance_receiver.is_some() {
            return Err(Error::Receiver);
        }
        let alignment = config.granule_alignment();
        if initial.len() > config.regions.saturating_mul(config.allocations)
            || !alignment.is_aligned(ByteLength::new(capacity))
        {
            return Err(Error::Capacity);
        }
        let mut regions = 0;
        let mut committed = 0u64;
        let mut total = ByteLength::new(0);
        for description in self.after_root(&plan) {
            regions += 1;
            if regions > config.regions {
                return Err(Error::Capacity);
            }
            config.geometry(description)?;
            total = total
                .checked_add(length(description.physical))
                .ok_or(Error::Capacity)?;
            let mut count = 0;
            for (index, item) in initial.iter().enumerate() {
                if !description.physical.covers(item.physical) {
                    continue;
                }
                if item.receiver.is_some()
                    || !item.physical.start().is_aligned(alignment)
                    || !item.physical.end().is_aligned(alignment)
                {
                    return Err(Error::Receiver);
                }
                if initial[..index]
                    .iter()
                    .any(|prior| prior.physical.overlaps(item.physical))
                {
                    return Err(Error::Overlap);
                }
                committed = committed
                    .checked_add(length(item.physical).get())
                    .ok_or(Error::Capacity)?;
                count += 1;
            }
            if count > config.allocations {
                return Err(Error::AllocationCapacity);
            }
        }
        if total > config.maximum || ByteLength::new(capacity) > total || committed > capacity {
            return Err(Error::Balance);
        }
        if initial.iter().any(|item| {
            !self
                .after_root(&plan)
                .any(|region| region.physical.covers(item.physical))
        }) {
            return Err(Error::Range);
        }

        let storage = self.take_root(mapping, plan);
        let source = initialize(
            storage,
            config,
            self.entries(),
            capacity,
            committed,
            allowance_receiver,
            initial,
        );
        self.clear();
        Ok(Some(source))
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

/// All accesses use disjoint prefixes of the one permanent mapped grant.
struct Carve {
    base: *mut MaybeUninit<u8>,
    cursor: Bump,
}

impl Carve {
    fn take<T>(&mut self, count: usize) -> &'static mut [MaybeUninit<T>] {
        let layout = array::<T>(count).expect("preflighted array");
        let offsets = self
            .cursor
            .reserve(layout)
            .expect("preflighted root layout");
        // Root base is aligned for all components. Bump proves disjoint
        // initialized or uninitialized storage and preserves pointer provenance.
        unsafe { slice::from_raw_parts_mut(self.base.add(offsets.start).cast(), count) }
    }
}

fn filled<T>(slice: &mut [MaybeUninit<T>]) -> &mut [T] {
    // All callers initialize every element in place before this conversion.
    unsafe { slice::from_raw_parts_mut(slice.as_mut_ptr().cast(), slice.len()) }
}

fn initialize(
    storage: RootStorage<'static>,
    config: Config,
    descriptions: impl Iterator<Item = Description>,
    capacity: u64,
    committed: u64,
    allowance_receiver: &mut Option<Allowance>,
    initial: &mut [Initial<'_>],
) -> &'static Allocator {
    let reserved = storage.reserved();
    let bytes = storage.into_bytes();
    let mut carve = Carve {
        base: bytes.as_mut_ptr(),
        cursor: Bump::new(0..bytes.len()).expect("nonempty root"),
    };
    let place = &mut carve.take::<Allocator>(1)[0];
    let address = ptr::from_mut(place).addr();
    let regions = carve.take::<Region>(config.regions);
    let allowances = carve.take::<TurnLock<Slot>>(config.allowances);
    for (index, slot) in allowances.iter_mut().enumerate() {
        let (generation, state) = if index == 0 {
            (
                1,
                SlotState::Open(Balance {
                    capacity,
                    reserved: 0,
                    occupied: committed,
                }),
            )
        } else {
            (
                0,
                SlotState::Free((index + 1 < config.allowances).then_some(index + 1)),
            )
        };
        slot.write(TurnLock::new(Slot { generation, state }));
    }

    for (index, region) in regions.iter_mut().enumerate() {
        let roots = carve.take::<Root>(config.roots);
        let nodes = carve.take::<u8>(config.nodes().expect("preflighted node count"));
        nodes.fill(MaybeUninit::new(0));
        let allocations = carve.take::<Entry<AllocationRecord>>(config.allocations);
        allocations.fill_with(|| MaybeUninit::new(Entry::EMPTY));
        let returns = carve.take::<Entry<ReturnRecord>>(config.returns);
        returns.fill_with(|| MaybeUninit::new(Entry::EMPTY));
        region.write(Region::new(
            Backing {
                roots,
                nodes: filled(nodes),
            },
            filled(allocations),
            filled(returns),
            address,
            index,
        ));
    }

    let regions = filled(regions);
    let allowance = super::AllowanceId {
        source: address,
        index: 0,
        generation: 1,
    };
    let mut charged = capacity;
    let mut published = 0;
    for (index, description) in descriptions.enumerate() {
        let region = &mut regions[index];
        let geometry = config.geometry(description).expect("preflighted Region");
        region.initialize(description, geometry, config.edits);
        let state = region.state.get_mut();
        let amount = charged.min(state.unassigned);
        state.unassigned -= amount;
        charged -= amount;
        for item in initial
            .iter_mut()
            .filter(|x| description.physical.covers(x.physical))
        {
            state
                .buddy()
                .occupy(item.physical)
                .expect("initial ranges preflighted");
            item.staged =
                Some(state.record(RegionId(index), item.physical, allowance, Phase::Issued));
        }
        published += 1;
    }

    let source = place.write(Allocator {
        config,
        root: reserved,
        directory: TurnLock::new(()),
        published: AtomicUsize::new(0),
        regions,
        allowances: filled(allowances),
        free: TurnLock::new(Free {
            head: (config.allowances > 1).then_some(1),
            retired: 0,
        }),
    });
    for item in initial {
        let id = item.staged.take().expect("preflighted initial position");
        *item.receiver = Some(Allocation::issued(source, id, item.physical));
    }
    *allowance_receiver = Some(Allowance::initial(source, allowance));
    source.published.store(published, Release);
    source
}

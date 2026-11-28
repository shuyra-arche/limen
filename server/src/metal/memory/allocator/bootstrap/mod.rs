//! Preflight and initialize the source’s permanent root and initial material.
pub(super) mod bump;

use core::{
    alloc::Layout,
    mem::MaybeUninit,
    ptr, slice,
    sync::atomic::{AtomicUsize, Ordering::Release},
};
use limen_treaty::model::{Alignment, ByteLength, PhysicalRange};

use crate::metal::{
    identity::treaty::{Entry, Generation, Slot},
    memory::access::treaty::ram::MappedRange,
    sync::treaty::TurnLock,
};

use super::{
    Allocator, AllowanceId, Error, Region, RegionDescription, RegionId, RegionOrigin, SourceId,
    allocation::{Allocation, AllocationId, AllocationRecord, Phase},
    allowance::{Allowance, AllowanceSlot, Balance, Free},
    directory::{Directory, Link},
    region::{
        Backing,
        buddy::{Geometry, Root},
    },
    returns::ReturnRecord,
};

use bump::Bump;

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
    /// Maximum Buddy nodes visited by one exact edit, checked against each Region's geometry.
    pub edits: usize,
}

impl Config {
    pub(super) fn check(self) -> Result<(), Error> {
        let alignment = Alignment::new(self.granule.get()).ok_or(Error::Layout)?;
        if !alignment.is_aligned(self.maximum) {
            return Err(Error::Layout);
        }
        // Directory capacity follows actual root storage and identity geometry.
        if self.regions == 0
            || Slot::new(self.regions - 1).is_none()
            || self.roots == 0
            || self.grains == 0
            || self.allocations == 0
            || self.returns < self.allocations
            || self.allowances == 0
            || Slot::new(self.allocations - 1).is_none()
            || Slot::new(self.returns - 1).is_none()
            || Slot::new(self.allowances - 1).is_none()
        {
            return Err(Error::Capacity);
        }
        Ok(())
    }

    pub(super) fn granule_alignment(self) -> Alignment {
        Alignment::new(self.granule.get()).expect("validated Allocator configuration")
    }

    pub(super) fn nodes(self) -> Result<usize, Error> {
        self.grains.checked_mul(2).ok_or(Error::Capacity)
    }

    pub(super) fn geometry(self, description: RegionDescription) -> Result<Geometry, Error> {
        let geometry =
            Geometry::new(description.physical, self.granule).map_err(|_| Error::Range)?;
        if description.physical.length().get() / self.granule.get() > self.grains as u64
            || geometry.root_count() > self.roots
            || geometry.edit_bound() > self.edits
        {
            return Err(Error::Capacity);
        }
        Ok(geometry)
    }
}

pub(super) fn array<T>(count: usize) -> Result<Layout, Error> {
    Layout::array::<T>(count).map_err(|_| Error::Layout)
}

/// Storage reserved for a root layout. Its entire backing granules have
/// already been removed from the geometry.
pub struct RootStorage<'a> {
    mapping: MappedRange<'a>,
    reserved: PhysicalRange,
    origin: RegionOrigin,
}

impl<'a> RootStorage<'a> {
    pub fn physical(&self) -> PhysicalRange {
        self.mapping.physical()
    }

    pub fn reserved(&self) -> PhysicalRange {
        self.reserved
    }

    pub fn origin(&self) -> RegionOrigin {
        self.origin
    }

    fn into_bytes(self) -> &'a mut [MaybeUninit<u8>] {
        self.mapping.into_bytes()
    }

    pub fn bytes(&mut self) -> &mut [MaybeUninit<u8>] {
        self.mapping.bytes()
    }
}

/// A planned reservation backed by a unique, unchanged mapped extent. The
/// original mapping remains with its owner until the reservation is consumed.
pub(crate) struct RootReservation<'r, 'm> {
    mapping: &'r mut Option<MappedRange<'m>>,
    layout: Layout,
    granule: Alignment,
    physical: PhysicalRange,
    reserved: PhysicalRange,
    origin: RegionOrigin,
}

impl<'r, 'm> RootReservation<'r, 'm> {
    pub(crate) fn prepare(
        mapping: &'r mut Option<MappedRange<'m>>,
        layout: Layout,
        granule: Alignment,
        region: RegionDescription,
    ) -> Result<Option<Self>, Error> {
        let Some(mapped) = mapping.as_ref() else {
            return Ok(None);
        };
        if !region.physical.covers(mapped.physical()) {
            return Ok(None);
        }
        let address = mapped.address();
        let limit = address.checked_add(mapped.length()).ok_or(Error::Range)?;
        if layout.size() == 0
            || mapped.physical().start().get() % layout.align() as u64
                != (address % layout.align()) as u64
        {
            return Err(Error::Layout);
        }
        let bytes = Bump::new(address..limit)
            .and_then(|mut bump| bump.reserve(layout))
            .ok_or(Error::Layout)?;
        let offset = u64::try_from(bytes.start - address).map_err(|_| Error::Range)?;
        let length = u64::try_from(bytes.len()).map_err(|_| Error::Range)?;
        let physical = mapped
            .physical()
            .start()
            .advance(ByteLength::new(offset))
            .and_then(|start| start.range(ByteLength::new(length)))
            .ok_or(Error::Range)?;
        let reserved = PhysicalRange::new(
            physical.start().align_down(granule),
            physical.end().align_up(granule).ok_or(Error::Range)?,
        )
        .filter(|range| region.physical.covers(*range))
        .ok_or(Error::Range)?;
        Ok(Some(Self {
            mapping,
            layout,
            granule,
            physical,
            reserved,
            origin: region.origin,
        }))
    }

    pub(crate) fn reserved(&self) -> PhysicalRange {
        self.reserved
    }

    pub(crate) fn reserve(self) -> RootStorage<'m> {
        let mut storage = self.mapping.take().expect("retained mapped range");
        let start = storage.physical().start();
        if self.physical.start() != start {
            let offset = self
                .physical
                .start()
                .distance(start)
                .expect("checked start");
            let (_, suffix) = storage.split(offset).ok().expect("checked root start");
            storage = suffix;
        }
        if self.physical.end() != storage.physical().end() {
            let (prefix, _) = storage
                .split(self.physical.length())
                .ok()
                .expect("checked root end");
            storage = prefix;
        }
        RootStorage {
            mapping: storage,
            reserved: self.reserved,
            origin: self.origin,
        }
    }
}

/// Exact geometry of one initial source after carving its fixed root.
/// Both slices remain borrowed through installation.
pub(crate) struct InitialRegions<'a> {
    pub(crate) before: &'a [Option<RegionDescription>],
    pub(crate) remainder: [Option<RegionDescription>; 2],
    pub(crate) after: &'a [Option<RegionDescription>],
}

impl InitialRegions<'_> {
    fn entries(&self) -> impl Iterator<Item = RegionDescription> + '_ {
        self.before
            .iter()
            .chain(self.remainder.iter())
            .chain(self.after.iter())
            .flatten()
            .copied()
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

/// Exact root input and initial receiving positions checked before Entry
/// consumes the permanent mapped storage.
pub(crate) struct RootInitialization<'r, 'a> {
    config: Config,
    root: RootReservation<'r, 'static>,
    regions: InitialRegions<'r>,
    capacity: u64,
    committed: u64,
    allowance_receiver: &'r mut Option<Allowance>,
    initial: &'r mut [Initial<'a>],
}

impl<'r, 'a> RootInitialization<'r, 'a> {
    pub(crate) fn prepare(
        config: Config,
        root: RootReservation<'r, 'static>,
        regions: InitialRegions<'r>,
        capacity: u64,
        allowance_receiver: &'r mut Option<Allowance>,
        initial: &'r mut [Initial<'a>],
    ) -> Result<Self, Error> {
        let layout = config.layout()?;
        if allowance_receiver.is_some() {
            return Err(Error::Receiver);
        }
        let alignment = config.granule_alignment();
        if root.layout != layout || root.granule != alignment {
            return Err(Error::Layout);
        }
        if initial.len() > config.regions.saturating_mul(config.allocations)
            || !alignment.is_aligned(ByteLength::new(capacity))
        {
            return Err(Error::Capacity);
        }
        let mut committed = 0u64;
        let mut total = ByteLength::new(0);
        let mut previous: Option<PhysicalRange> = None;
        for (index, description) in regions.entries().enumerate() {
            if description.physical.overlaps(root.reserved) {
                return Err(Error::Overlap);
            }
            if index >= config.regions {
                return Err(Error::Capacity);
            }
            if previous.is_some_and(|prior| prior.end() > description.physical.start()) {
                return Err(Error::Overlap);
            }
            previous = Some(description.physical);
            config.geometry(description)?;
            total = total
                .checked_add(description.physical.length())
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
                    .checked_add(item.physical.length().get())
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
            !regions
                .entries()
                .any(|region| region.physical.covers(item.physical))
        }) {
            return Err(Error::Range);
        }

        Ok(Self {
            config,
            root,
            regions,
            capacity,
            committed,
            allowance_receiver,
            initial,
        })
    }

    /// # Safety
    ///
    /// The caller is the exclusive authority to hand off all regions to
    /// this source once, and keeps them isolated from other physical use
    /// through admission. The retained mapping must be canonical permanent
    /// storage. Receiver slots belong to the actual initial consumers and
    /// remain recoverable after installation.
    pub(crate) unsafe fn install(self) -> &'static Allocator {
        initialize(
            self.root.reserve(),
            self.config,
            self.regions.entries(),
            self.capacity,
            self.committed,
            self.allowance_receiver,
            self.initial,
        )
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
    descriptions: impl Iterator<Item = RegionDescription>,
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
    let source_id = SourceId(ptr::from_mut(place).addr());
    let regions = carve.take::<Region>(config.regions);
    let links = carve.take::<Link>(config.regions);
    links.fill(MaybeUninit::new(Link::EMPTY));
    let mut directory = Directory::new(filled(links));
    let allowances = carve.take::<TurnLock<AllowanceSlot>>(config.allowances);
    for (index, slot) in allowances.iter_mut().enumerate() {
        let entry = if index == 0 {
            AllowanceSlot::initial(Balance::new(capacity, committed))
        } else {
            let next = (index + 1 < config.allowances)
                .then(|| Slot::new(index + 1).expect("checked Allowance capacity"));
            AllowanceSlot::free(next)
        };
        slot.write(TurnLock::new(entry));
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
            RegionId::new(source_id, index),
        ));
    }

    let regions = filled(regions);
    let initial_slot = Slot::new(0).expect("initial Allowance slot");
    let allowance = AllowanceId::new(source_id, initial_slot, Generation::FIRST);
    let mut charged = capacity;
    let mut published = 0;
    for (index, description) in descriptions.enumerate() {
        let plan = directory
            .prepare(&regions[..index], description.physical, config.maximum)
            .expect("preflighted disjoint Region list");
        let region = &mut regions[index];
        let geometry = config.geometry(description).expect("preflighted Region");
        region.initialize(description, geometry, config.edits);
        let state = region.state.get_mut();
        let amount = charged.min(state.unassigned());
        state.set_unassigned(state.unassigned() - amount);
        charged -= amount;
        for item in initial
            .iter_mut()
            .filter(|x| description.physical.covers(x.physical))
        {
            state
                .buddy()
                .occupy(item.physical)
                .expect("initial ranges preflighted");
            item.staged = Some(state.record(item.physical, allowance, Phase::Issued));
        }
        directory.insert(Slot::new(index).expect("checked Region slot"), plan);
        published += 1;
    }

    let source = place.write(Allocator {
        config,
        root: reserved,
        directory: TurnLock::new(directory),
        published: AtomicUsize::new(0),
        regions,
        allowances: filled(allowances),
        free: TurnLock::new(Free {
            head: (config.allowances > 1).then(|| Slot::new(1).expect("second Allowance slot")),
            retired: 0,
        }),
    });
    for item in initial {
        let id = item.staged.take().expect("preflighted initial position");
        *item.receiver = Some(Allocation::issued(source, id, item.physical, None));
    }
    *allowance_receiver = Some(Allowance::initial(source, allowance));
    source.published.store(published, Release);
    source
}

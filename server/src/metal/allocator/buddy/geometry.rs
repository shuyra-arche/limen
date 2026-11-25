//! Checked geometry and node locations for one physical region.

use core::{alloc::Layout, ops::Range};

use super::Error;

/// A checked, nonempty region in absolute physical bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Geometry {
    pub(super) region: Range<u64>,
    pub(super) granule: u64,
    roots: usize,
    nodes: usize,
    edit_bound: usize,
}

/// One naturally aligned root in the fixed forest table.
pub struct Root {
    pub(super) base: u64,
    pub(super) order: u32,
    pub(super) first: usize,
}

impl Geometry {
    pub fn new(region: Range<u64>, granule: u64) -> Result<Self, Error> {
        if !granule.is_power_of_two() {
            return Err(Error::Granule);
        }
        if region.is_empty()
            || !region.start.is_multiple_of(granule)
            || !region.end.is_multiple_of(granule)
        {
            return Err(Error::Range);
        }

        let first = region.start / granule;
        let end = region.end / granule;
        let mut roots = 0usize;
        let mut height = 0usize;
        for (_, order) in blocks(first..end) {
            roots = roots.checked_add(1).ok_or(Error::Capacity)?;
            height = height.max(order as usize);
        }
        let grains = usize::try_from(end - first).map_err(|_| Error::Capacity)?;
        // Count leaves and internal nodes separately so 2 * grains need not fit.
        let nodes = grains.checked_add(grains - roots).ok_or(Error::Capacity)?;
        Layout::array::<Root>(roots).map_err(|_| Error::Capacity)?;
        Layout::array::<u8>(nodes).map_err(|_| Error::Capacity)?;
        // Both passes visit each root once. At each depth, each of the two
        // boundaries can cause up to two child visits in each pass.
        let edit_bound = roots
            .checked_mul(2)
            .and_then(|root_visits| root_visits.checked_add(8 * height))
            .ok_or(Error::Capacity)?;
        Ok(Self {
            region,
            granule,
            roots,
            nodes,
            edit_bound,
        })
    }

    pub const fn root_count(&self) -> usize {
        self.roots
    }

    /// Each node uses one byte. The forest has 2N - B nodes.
    pub const fn node_count(&self) -> usize {
        self.nodes
    }

    /// Maximum number of node visits in validation and rewriting combined.
    /// Each visit uses a fixed number of arithmetic and state operations.
    pub const fn edit_bound(&self) -> usize {
        self.edit_bound
    }

    /// A complete scan visits and advances past each effective node once.
    pub const fn scan_bound(&self) -> usize {
        // A validated node array is no larger than isize::MAX bytes.
        2 * self.nodes
    }

    pub(super) fn roots(&self) -> impl Iterator<Item = Root> {
        let mut first = 0;
        blocks(self.region.start / self.granule..self.region.end / self.granule).map(
            move |(base, order)| {
                let root = Root { base, order, first };
                // The aggregate node count and the array layout were checked.
                let grains = (1u64 << order) as usize;
                first += grains + (grains - 1);
                root
            },
        )
    }

    pub(super) fn grains(&self, physical: Range<u64>) -> Result<Range<u64>, Error> {
        if physical.is_empty()
            || physical.start < self.region.start
            || physical.end > self.region.end
            || !physical.start.is_multiple_of(self.granule)
            || !physical.end.is_multiple_of(self.granule)
        {
            return Err(Error::Range);
        }
        Ok(physical.start / self.granule..physical.end / self.granule)
    }
}

fn blocks(mut grains: Range<u64>) -> impl Iterator<Item = (u64, u32)> {
    core::iter::from_fn(move || {
        if grains.is_empty() {
            return None;
        }
        let base = grains.start;
        // trailing_zeros(0) is the word width, so the remaining length
        // alone bounds the first root when physical granule zero is present.
        let order = base.trailing_zeros().min((grains.end - base).ilog2());
        grains.start += 1u64 << order;
        Some((base, order))
    })
}

/// A node's root-local heap index and absolute granule location.
#[derive(Clone, Copy)]
pub(super) struct Block {
    pub(super) index: usize,
    pub(super) base: u64,
    pub(super) order: u32,
}

impl Root {
    pub(super) fn block(&self) -> Block {
        Block {
            index: 0,
            base: self.base,
            order: self.order,
        }
    }
}

impl Block {
    pub(super) fn end(self) -> u64 {
        self.base + (1u64 << self.order)
    }

    pub(super) fn left(self) -> Self {
        // The checked full tree contains both children of a nonleaf.
        Self {
            index: 2 * self.index + 1,
            base: self.base,
            order: self.order - 1,
        }
    }

    pub(super) fn right(self) -> Self {
        self.left().sibling()
    }

    pub(super) fn sibling(self) -> Self {
        // The caller is positioned at a left child.
        Self {
            index: self.index + 1,
            base: self.end(),
            order: self.order,
        }
    }

    pub(super) fn parent(self) -> Self {
        // The caller has finished a right child.
        Self {
            index: (self.index - 1) / 2,
            base: self.base - (1u64 << self.order),
            order: self.order + 1,
        }
    }
}

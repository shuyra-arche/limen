//! A fixed Buddy forest for one already checked physical region.
//!
//! The owner supplies exclusive metadata storage and serializes mutation.
//! The forest tracks physical occupancy. The allocator owns the corresponding
//! allocation records, authorization, accounting and retirement.

mod geometry;
mod scan;

use core::{mem::MaybeUninit, ops::Range, slice};

pub use geometry::{Geometry, Root};
pub use scan::{Request, Revision, Scan, Search};

use geometry::Block;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Granule,
    Range,
    Capacity,
    EditLimit,
    Layout,
    State,
    Foreign,
    Changed,
}

pub struct Buddy<'storage> {
    geometry: Geometry,
    roots: &'storage [Root],
    nodes: &'storage mut [u8],
    revision: Revision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Occupied,
    Free,
    Mixed(u32),
}

impl State {
    fn encode(self) -> u8 {
        match self {
            Self::Occupied => 0,
            Self::Free => 1,
            Self::Mixed(largest) => 2 + largest as u8,
        }
    }

    fn decode(byte: u8, order: u32) -> Self {
        match byte {
            0 => Self::Occupied,
            1 => Self::Free,
            byte => {
                let largest = u32::from(byte - 2);
                assert!(largest < order, "invalid mixed Buddy node");
                Self::Mixed(largest)
            }
        }
    }

    fn largest(self, order: u32) -> Option<u32> {
        match self {
            Self::Occupied => None,
            Self::Free => Some(order),
            Self::Mixed(largest) => Some(largest),
        }
    }

    fn join(left: Self, right: Self, order: u32) -> Self {
        match (left, right) {
            (Self::Occupied, Self::Occupied) => Self::Occupied,
            (Self::Free, Self::Free) => Self::Free,
            _ => Self::Mixed(
                left.largest(order)
                    .max(right.largest(order))
                    .expect("mixed has free space"),
            ),
        }
    }
}

impl<'storage> Buddy<'storage> {
    /// Initialize an entirely free, private forest in fixed storage.
    ///
    /// The owner excludes metadata and all other retained physical ranges
    /// before accepting the region. To publish initial occupied allocations,
    /// mark their exact ranges while the region is still private.
    pub fn new(
        geometry: Geometry,
        roots: &'storage mut [MaybeUninit<Root>],
        nodes: &'storage mut [MaybeUninit<u8>],
        edit_limit: usize,
    ) -> Result<Self, Error> {
        if geometry.edit_bound() > edit_limit {
            return Err(Error::EditLimit);
        }
        let roots = roots
            .get_mut(..geometry.root_count())
            .ok_or(Error::Capacity)?;
        let nodes = nodes
            .get_mut(..geometry.node_count())
            .ok_or(Error::Capacity)?;

        // Initialize inactive bytes for a safe byte slice. No inactive
        // descendant is interpreted until its parent is split.
        for byte in nodes.iter_mut() {
            byte.write(State::Occupied.encode());
        }
        for (slot, root) in roots.iter_mut().zip(geometry.roots()) {
            nodes[root.first].write(State::Free.encode());
            slot.write(root);
        }
        // Both exact prefixes are fully initialized. Root geometry remains
        // immutable for this instance, including while scans hold a borrow.
        let roots = unsafe { slice::from_raw_parts(roots.as_ptr().cast::<Root>(), roots.len()) };
        let nodes =
            unsafe { slice::from_raw_parts_mut(nodes.as_mut_ptr().cast::<u8>(), nodes.len()) };
        Ok(Self {
            geometry,
            roots,
            nodes,
            revision: Revision::Current(0),
        })
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn largest_free_order(&self) -> Option<u32> {
        self.roots
            .iter()
            .filter_map(|root| self.state(root.first, root.block()).largest(root.order))
            .max()
    }

    /// Circle exactly this free range. The result counts structural visits.
    pub fn occupy(&mut self, physical: Range<u64>) -> Result<usize, Error> {
        self.edit(physical, State::Free, State::Occupied)
    }

    /// Return exactly this occupied range. The owner validates its ownership
    /// and retirement before calling. The result counts structural visits.
    pub fn release(&mut self, physical: Range<u64>) -> Result<usize, Error> {
        self.edit(physical, State::Occupied, State::Free)
    }

    fn edit(&mut self, physical: Range<u64>, old: State, new: State) -> Result<usize, Error> {
        let range = self.geometry.grains(physical)?;
        let mut visits = 0;
        for root in self.roots {
            if !self.matches(root.first, root.block(), &range, old, &mut visits) {
                return Err(Error::State);
            }
        }
        for root in self.roots {
            visits += self.rewrite(root.first, root.block(), &range, new);
        }
        self.revision = self.revision.next();
        Ok(visits)
    }

    fn matches(
        &self,
        first: usize,
        block: Block,
        range: &Range<u64>,
        expected: State,
        visits: &mut usize,
    ) -> bool {
        *visits += 1;
        if outside(block, range) {
            return true;
        }
        let state = self.state(first, block);
        match state {
            State::Free | State::Occupied => state == expected,
            State::Mixed(_) => {
                // A wholly covered mixed node necessarily has both states.
                !covered(block, range)
                    && self.matches(first, block.left(), range, expected, visits)
                    && self.matches(first, block.right(), range, expected, visits)
            }
        }
    }

    fn rewrite(
        &mut self,
        first: usize,
        block: Block,
        range: &Range<u64>,
        replacement: State,
    ) -> usize {
        if outside(block, range) {
            return 1;
        }
        if covered(block, range) {
            self.set(first, block, replacement);
            return 1;
        }

        let left = block.left();
        let right = block.right();
        let old = self.state(first, block);
        if !matches!(old, State::Mixed(_)) {
            self.set(first, left, old);
            self.set(first, right, old);
        }
        let left_visits = self.rewrite(first, left, range, replacement);
        let right_visits = self.rewrite(first, right, range, replacement);
        self.set(
            first,
            block,
            State::join(
                self.state(first, left),
                self.state(first, right),
                left.order,
            ),
        );
        1 + left_visits + right_visits
    }

    fn state(&self, first: usize, block: Block) -> State {
        State::decode(self.nodes[first + block.index], block.order)
    }

    fn set(&mut self, first: usize, block: Block, state: State) {
        self.nodes[first + block.index] = state.encode();
    }
}

fn outside(block: Block, range: &Range<u64>) -> bool {
    block.end() <= range.start || block.base >= range.end
}

fn covered(block: Block, range: &Range<u64>) -> bool {
    range.start <= block.base && block.end() <= range.end
}

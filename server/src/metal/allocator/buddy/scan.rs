//! Summary search and budgeted, revision-checked physical-order traversal.

use core::ptr;
use limen_treaty::model::{Alignment, ByteLength, PhysicalAddress, PhysicalRange};


use super::{Buddy, Error, Root, State, geometry::Block};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    length: ByteLength,
    alignment: Alignment,
    window: Option<PhysicalRange>,
}

impl Request {
    pub fn new(length: ByteLength, alignment: Alignment) -> Result<Self, Error> {
        if length.get() == 0 {
            return Err(Error::Layout);
        }
        Ok(Self {
            length,
            alignment,
            window: PhysicalAddress::new(0).range(ByteLength::new(u64::MAX)),
        })
    }

    /// Narrow eligibility to the intersection with the preceding window.
    pub fn restrict(mut self, window: PhysicalRange) -> Self {
        self.window = self.window.and_then(|current| current.intersect(window));
        self
    }

    pub(in super::super) fn length(self) -> ByteLength {
        self.length
    }

    pub(in super::super) fn alignment(self) -> Alignment {
        self.alignment
    }

    pub(in super::super) fn window(self) -> Option<PhysicalRange> {
        self.window
    }

    fn validate(self, granule: Alignment) -> Result<(), Error> {
        if !granule.is_aligned(self.length) {
            return Err(Error::Layout);
        }
        Ok(())
    }

    fn within(
        self,
        start: PhysicalAddress,
        stop: PhysicalAddress,
        granule: Alignment,
    ) -> Option<PhysicalRange> {
        let window = self.window?;
        let start = start
            .max(window.start())
            .align_up(self.alignment.max(granule))?;
        let candidate = start.range(self.length)?;
        (candidate.end() <= stop.min(window.end())).then_some(candidate)
    }
}

/// A scan revision never wraps. Exhaustion prevents stitched observations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Revision {
    Current(u64),
    Exhausted,
}

impl Revision {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Current(version) if version != u64::MAX => Self::Current(version + 1),
            _ => Self::Exhausted,
        }
    }

    pub(crate) fn preserves(self, observed: Self) -> bool {
        matches!((self, observed), (Self::Current(now), Self::Current(before)) if now == before)
    }
}

/// A result is local to this physical region and the observed revision.
pub enum Search<'storage> {
    Found(PhysicalRange),
    Pending(Scan<'storage>),
    /// Exhausted can certify a complete single observation only.
    Missing(Revision),
}

/// Inactive search state. The immutable roots pin the originating table,
/// while nodes may change before the owner resumes under its protection.
pub struct Scan<'storage> {
    roots: &'storage [Root],
    revision: Revision,
    request: Request,
    root: usize,
    block: Block,
    phase: Phase,
    run: Option<PhysicalAddress>,
}

enum Phase {
    Visit,
    Advance,
}

impl<'storage> Buddy<'storage> {
    /// A summary hit supplies an exact candidate. A miss is inconclusive.
    pub fn fast(&self, request: Request) -> Result<Option<PhysicalRange>, Error> {
        let granule = self.geometry.granule;
        request.validate(granule)?;
        let grains = request.length.get().max(request.alignment.get()) / granule.get();
        let Some(required) = grains.checked_next_power_of_two().map(u64::ilog2) else {
            return Ok(None);
        };

        for root in self.roots {
            let mut block = root.block();
            let mut state = self.state(root.first, block);
            if !state
                .largest(block.order)
                .is_some_and(|largest| largest >= required)
            {
                continue;
            }
            while state != State::Free {
                let left = block.left();
                let left_state = self.state(root.first, left);
                if left_state
                    .largest(left.order)
                    .is_some_and(|largest| largest >= required)
                {
                    block = left;
                    state = left_state;
                } else {
                    block = block.right();
                    state = self.state(root.first, block);
                }
            }
            let start = self.geometry.address(block.base);
            let stop = self.geometry.address(block.end());
            return Ok(request.within(start, stop, granule));
        }
        Ok(None)
    }

    /// Start at the lowest physical address. A step is one node inspection
    /// or one cursor advance. Each step has a fixed amount of additional work.
    pub fn scan(&self, request: Request, budget: usize) -> Result<Search<'storage>, Error> {
        request.validate(self.geometry.granule)?;
        let scan = Scan {
            roots: self.roots,
            revision: self.revision,
            request,
            root: 0,
            block: self.roots[0].block(),
            phase: Phase::Visit,
            run: None,
        };
        Ok(self.search(scan, budget))
    }

    /// Resume only against the exact table and the same valid revision.
    pub fn resume(&self, scan: Scan<'storage>, budget: usize) -> Result<Search<'storage>, Error> {
        if !ptr::eq(self.roots, scan.roots) {
            return Err(Error::Foreign);
        }
        if !self.revision.preserves(scan.revision) {
            return Err(Error::Changed);
        }
        Ok(self.search(scan, budget))
    }

    fn search(&self, mut scan: Scan<'storage>, budget: usize) -> Search<'storage> {
        let granule = self.geometry.granule;
        for _ in 0..budget {
            let block = scan.block;
            match scan.phase {
                Phase::Visit => match self.state(self.roots[scan.root].first, block) {
                    State::Free => {
                        let start = *scan.run.get_or_insert(self.geometry.address(block.base));
                        let stop = self.geometry.address(block.end());
                        if let Some(found) = scan.request.within(start, stop, granule) {
                            return Search::Found(found);
                        }
                        scan.phase = Phase::Advance;
                    }
                    State::Occupied => {
                        scan.run = None;
                        scan.phase = Phase::Advance;
                    }
                    State::Mixed(_) => scan.block = block.left(),
                },
                Phase::Advance if block.index == 0 => {
                    scan.root += 1;
                    let Some(root) = self.roots.get(scan.root) else {
                        return Search::Missing(scan.revision);
                    };
                    scan.block = root.block();
                    scan.phase = Phase::Visit;
                }
                Phase::Advance if block.index % 2 == 1 => {
                    scan.block = block.sibling();
                    scan.phase = Phase::Visit;
                }
                Phase::Advance => scan.block = block.parent(),
            }
        }
        Search::Pending(scan)
    }
}

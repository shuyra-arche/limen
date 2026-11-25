//! Summary search and budgeted, revision-checked physical-order traversal.

use core::{ops::Range, ptr};

use super::{Buddy, Error, Root, State, geometry::Block};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    length: u64,
    alignment: u64,
    first: u64,
    limit: u64,
}

impl Request {
    pub fn new(length: u64, alignment: u64) -> Result<Self, Error> {
        if length == 0 || !alignment.is_power_of_two() {
            return Err(Error::Layout);
        }
        Ok(Self {
            length,
            alignment,
            first: 0,
            limit: u64::MAX,
        })
    }

    pub fn restrict(mut self, window: Range<u64>) -> Result<Self, Error> {
        if window.is_empty() {
            return Err(Error::Range);
        }
        self.first = self.first.max(window.start);
        self.limit = self.limit.min(window.end);
        Ok(self)
    }

    fn validate(self, granule: u64) -> Result<(), Error> {
        if !self.length.is_multiple_of(granule) {
            return Err(Error::Layout);
        }
        Ok(())
    }

    fn within(self, start: u64, stop: u64, granule: u64) -> Option<Range<u64>> {
        let start = start
            .max(self.first)
            .checked_next_multiple_of(self.alignment.max(granule))?;
        let end = start.checked_add(self.length)?;
        (end <= stop.min(self.limit)).then_some(start..end)
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
    Found(Range<u64>),
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
    run: Option<u64>,
}

enum Phase {
    Visit,
    Advance,
}

impl<'storage> Buddy<'storage> {
    /// A summary hit supplies an exact candidate. A miss is inconclusive.
    pub fn fast(&self, request: Request) -> Result<Option<Range<u64>>, Error> {
        let granule = self.geometry.granule;
        request.validate(granule)?;
        let grains = (request.length / granule).max(request.alignment / granule);
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
            let start = block.base * granule;
            return Ok(request.within(start, block.end() * granule, granule));
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
                        let start = *scan.run.get_or_insert(block.base * granule);
                        if let Some(found) =
                            scan.request.within(start, block.end() * granule, granule)
                        {
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

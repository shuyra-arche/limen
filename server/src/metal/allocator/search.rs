use core::sync::atomic::Ordering::Acquire;

use crate::{
    entry::boot::{Physical, RegionSource},
    sync::{Acquisition, TurnLock, Work},
};

use super::{
    Allocator, Description, Error, REGION_LIMIT, RegionId,
    buddy::{self, Revision, Scan},
    length, overlaps, range,
};

/// The recipient's intended use, independent of the physical range selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RamPurpose {
    Metadata,
    Execution,
    PageTable,
    Cell,
}

/// Fixed purpose, layout and physical eligibility for one lookup. Actual
/// spending still revalidates these facts under the authorization's protection.
#[derive(Clone, Copy, Debug)]
pub struct Request {
    buddy: buddy::Request,
    length: u64,
    alignment: u64,
    window: Physical,
    attributes: Option<RegionSource>,
    purpose: RamPurpose,
}

impl Request {
    pub fn new(
        length: u64,
        alignment: u64,
        window: Physical,
        attributes: Option<RegionSource>,
        purpose: RamPurpose,
    ) -> Result<Self, Error> {
        let buddy = buddy::Request::new(length, alignment)
            .and_then(|request| request.restrict(range(window)))
            .map_err(|_| Error::Layout)?;
        Ok(Self {
            buddy,
            length,
            alignment,
            window,
            attributes,
            purpose,
        })
    }

    pub fn length(self) -> u64 {
        self.length
    }

    pub fn alignment(self) -> u64 {
        self.alignment
    }

    pub fn purpose(self) -> RamPurpose {
        self.purpose
    }

    fn accepts(self, region: Description) -> bool {
        overlaps(self.window, region.physical)
            && self
                .attributes
                .is_none_or(|attributes| attributes == region.source)
    }
}

/// Physical observation only. The candidate is rechecked during occupation.
#[derive(Clone, Copy)]
pub struct Candidate {
    pub(super) source: &'static Allocator,
    pub(super) region: RegionId,
    pub(super) physical: Physical,
    pub(super) request: Request,
}

impl Candidate {
    pub fn physical(self) -> Physical {
        self.physical
    }

    pub fn region(self) -> RegionId {
        self.region
    }

    pub fn request(self) -> Request {
        self.request
    }
}

pub enum Search {
    Pending,
    Candidate(Candidate),
    NoRange,
}

enum Pass {
    Observe,
    Candidate(Physical),
    Verify,
    Directory,
    Finished,
}

/// One exact source/request and fixed per-Region observation workspace. Each
/// step runs one bounded local operation and leaves no held lock behind.
#[must_use]
pub struct Lookup {
    source: &'static Allocator,
    request: Request,
    count: usize,
    first: usize,
    visited: usize,
    pass: Pass,
    scan: Option<Scan<'static>>,
    observations: [Option<Revision>; REGION_LIMIT],
    blocked: bool,
}

// A scan visit or cursor advance is one step. Fast lookup and exact edits
// already have the separate checked structural bound at Region admission.
const SCAN_STEPS: usize = 64;

impl Allocator {
    pub fn lookup(
        &'static self,
        request: Request,
        start: Option<RegionId>,
    ) -> Result<Lookup, Error> {
        if !request.length.is_multiple_of(self.config.granule) {
            return Err(Error::Layout);
        }
        // This is the first directory observation for a possible NoRange.
        let count = with_turn(&self.directory, |_| self.published.load(Acquire))?;
        let first = start.filter(|id| id.0 < count).map_or(0, |id| id.0);
        Ok(Lookup {
            source: self,
            request,
            count,
            first,
            visited: 0,
            pass: Pass::Observe,
            scan: None,
            observations: [None; REGION_LIMIT],
            blocked: false,
        })
    }
}

impl Lookup {
    pub fn step(&mut self) -> Result<Search, Error> {
        let result = match self.pass {
            Pass::Observe if self.visited == self.count => {
                if self.blocked {
                    Err(Error::Busy)
                } else {
                    self.visited = 0;
                    self.pass = Pass::Verify;
                    Ok(Search::Pending)
                }
            }
            Pass::Observe => self.observe(),
            Pass::Candidate(physical) => Ok(self.candidate(physical)),
            Pass::Verify if self.visited == self.count => {
                self.pass = Pass::Directory;
                Ok(Search::Pending)
            }
            Pass::Verify => self.verify(),
            Pass::Directory => {
                let source = self.source;
                with_turn(&source.directory, |_| source.published.load(Acquire)).and_then(|count| {
                    if count != self.count {
                        return Err(Error::Changed);
                    }
                    self.pass = Pass::Finished;
                    Ok(Search::NoRange)
                })
            }
            Pass::Finished => Err(Error::Stale),
        };
        if result.is_err() {
            self.pass = Pass::Finished;
        }
        result
    }

    /// Continue with the next Region after rejecting this lookup's outstanding
    /// candidate. Repeated steps yield that candidate until it is skipped.
    /// A rejected hit cannot contribute to NoRange.
    pub fn skip_current(&mut self) -> Result<(), Error> {
        if !matches!(self.pass, Pass::Candidate(_)) {
            return Err(Error::Stale);
        }
        self.blocked = true;
        self.visited += 1;
        self.pass = Pass::Observe;
        Ok(())
    }

    fn candidate(&self, physical: Physical) -> Search {
        Search::Candidate(Candidate {
            source: self.source,
            region: RegionId(self.index()),
            physical,
            request: self.request,
        })
    }

    fn index(&self) -> usize {
        (self.first + self.visited) % self.count
    }

    fn observe(&mut self) -> Result<Search, Error> {
        let index = self.index();
        let region = &self.source.regions[index];
        let description = region.description();
        if !self.request.accepts(description) || length(description.physical) < self.request.length
        {
            self.visited += 1;
            return Ok(Search::Pending);
        }

        let request = self.request.buddy;
        let scan = self.scan.take();
        let observation = with_turn(&region.state, |state| {
            let buddy = state.buddy();
            if let Some(scan) = scan {
                return buddy.resume(scan, SCAN_STEPS);
            }
            if let Some(physical) = buddy.fast(request)? {
                return Ok(buddy::Search::Found(physical));
            }
            buddy.scan(request, SCAN_STEPS)
        })?
        .map_err(|reason| match reason {
            buddy::Error::Changed => Error::Changed,
            _ => Error::Layout,
        })?;

        match observation {
            buddy::Search::Found(found) => {
                let physical = Physical {
                    start: found.start,
                    end: found.end,
                };
                self.pass = Pass::Candidate(physical);
                Ok(self.candidate(physical))
            }
            buddy::Search::Pending(scan) => {
                self.scan = Some(scan);
                Ok(Search::Pending)
            }
            buddy::Search::Missing(revision) => {
                if !revision.preserves(revision) {
                    return Err(Error::Changed);
                }
                self.observations[index] = Some(revision);
                self.visited += 1;
                Ok(Search::Pending)
            }
        }
    }

    fn verify(&mut self) -> Result<Search, Error> {
        let index = self.index();
        self.visited += 1;
        if let Some(observed) = self.observations[index] {
            let region = &self.source.regions[index];
            let current = with_turn(&region.state, |state| state.buddy().revision())?;
            if !current.preserves(observed) {
                return Err(Error::Changed);
            }
        }
        Ok(Search::Pending)
    }
}

pub(super) fn with_turn<T, R>(
    lock: &TurnLock<T>,
    operation: impl FnOnce(&mut T) -> R,
) -> Result<R, Error> {
    let mut work = Work::<1>::new();
    let mut plan = work.plan();
    plan.insert(lock).expect("one Turn");
    Ok(plan.execute(Acquisition::Fast, |held| {
        operation(held.access(lock).expect("planned lock"))
    })?)
}

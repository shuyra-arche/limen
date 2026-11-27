use core::sync::atomic::Ordering::Acquire;
use limen_treaty::model::PhysicalRange;

use super::{
    Allocator, Candidate, Error, RegionId,
    region::{
        Revision,
        buddy::{self, Scan},
    },
    treaty::{Request, Search},
};

enum Pass {
    Observe,
    Candidate(PhysicalRange),
    Verify,
    Directory,
    Finished,
}

/// One exact source/request and fixed per-Region observation workspace. Each
/// step runs one bounded local operation and leaves no held lock behind.
#[must_use]
pub struct Lookup<'work> {
    source: &'static Allocator,
    request: Request,
    count: usize,
    first: usize,
    visited: usize,
    pass: Pass,
    scan: Option<Scan<'static>>,
    observations: &'work mut [LookupObservation],
    blocked: bool,
}

/// Actual caller-provided storage for one Region's geometry observation.
#[derive(Clone, Copy)]
pub struct LookupObservation(Option<Revision>);

impl LookupObservation {
    pub const EMPTY: Self = Self(None);
}

// Fixed per-call bound for the resumable Buddy search under one Region Turn.
// A visit or cursor advance is one step. Fast lookups and exact edits have
// separate geometry bounds checked when that Region is admitted.
const SCAN_STEPS: usize = 64;

impl Allocator {
    pub fn lookup<'work>(
        &'static self,
        request: Request,
        start: Option<RegionId>,
        observations: &'work mut [LookupObservation],
    ) -> Result<Lookup<'work>, Error> {
        if !self.config.granule_alignment().is_aligned(request.length()) {
            return Err(Error::Layout);
        }
        if start.is_some_and(|id| id.source != self.source_id()) {
            return Err(Error::Stale);
        }
        // This is the first directory observation for a possible NoRange.
        let count = self.directory.acquire(|_| self.published.load(Acquire))?;
        let first = start.map_or(0, RegionId::index);
        if start.is_some() && first >= count {
            return Err(Error::Stale);
        }
        let observations = observations.get_mut(..count).ok_or(Error::Capacity)?;
        observations.fill(LookupObservation::EMPTY);
        Ok(Lookup {
            source: self,
            request,
            count,
            first,
            visited: 0,
            pass: Pass::Observe,
            scan: None,
            observations,
            blocked: false,
        })
    }
}

impl Lookup<'_> {
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
                source
                    .directory
                    .acquire(|_| source.published.load(Acquire))
                    .map_err(Error::from)
                    .and_then(|count| {
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

    fn candidate(&self, physical: PhysicalRange) -> Search {
        Search::Candidate(Candidate {
            region: RegionId::new(self.source.source_id(), self.index()),
            physical,
            request: self.request,
        })
    }

    fn index(&self) -> usize {
        let tail = self.count - self.first;
        if self.visited < tail {
            self.first + self.visited
        } else {
            self.visited - tail
        }
    }

    fn observe(&mut self) -> Result<Search, Error> {
        let index = self.index();
        let region = &self.source.regions[index];
        let description = region.description();
        if !self.request.accepts(description)
            || description.physical.length() < self.request.length()
        {
            self.visited += 1;
            return Ok(Search::Pending);
        }

        let request = self.request.buddy;
        let scan = self.scan.take();
        let observation = region
            .state
            .acquire(|state| {
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
            buddy::Search::Found(physical) => {
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
                self.observations[index] = LookupObservation(Some(revision));
                self.visited += 1;
                Ok(Search::Pending)
            }
        }
    }

    fn verify(&mut self) -> Result<Search, Error> {
        let index = self.index();
        self.visited += 1;
        if let Some(observed) = self.observations[index].0 {
            let region = &self.source.regions[index];
            let current = region.state.acquire(|state| state.buddy().revision())?;
            if !current.preserves(observed) {
                return Err(Error::Changed);
            }
        }
        Ok(Search::Pending)
    }
}

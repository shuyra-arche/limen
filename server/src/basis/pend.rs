//! Root allocation authority installed in the kernel's initial PendSpace.

use core::ptr;
use limen_treaty::model::{Alignment, ByteLength};

use crate::{
    allocator::{
        self, AllocationId, Allocator, Allowance, AllowanceId, Candidate, Construction, RamPurpose,
        Spender,
    },
    metal::sync::{TurnAccess, TurnAcquireError, TurnLock, TurnMode, TurnWorkspace},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Limit,
    Receiver,
    Installed,
    Revoked,
    Exhausted,
    Source(allocator::Error),
}

impl From<allocator::Error> for Error {
    fn from(error: allocator::Error) -> Self {
        Self::Source(error)
    }
}

impl From<TurnAcquireError> for Error {
    fn from(error: TurnAcquireError) -> Self {
        Self::Source(error.into())
    }
}

/// The actual source and share, bounded to metadata requests. No Cell or
/// initial Cell handle can be issued from this permit.
#[derive(Clone, Copy)]
struct AllocationPermit {
    source: &'static Allocator,
    allowance: AllowanceId,
    maximum: ByteLength,
    alignment: Alignment,
}

impl AllocationPermit {
    fn check(self, candidate: Candidate) -> Result<(), Error> {
        let request = candidate.request();
        if request.purpose() != RamPurpose::Metadata
            || request.length() > self.maximum
            || request.alignment() > self.alignment
        {
            return Err(Error::Limit);
        }
        Ok(())
    }
}

enum Admission {
    Open,
    Shut,
}

struct Pend {
    holding: Option<AllocationPermit>,
    branch: Admission,
}

enum State {
    Fresh,
    Published(Pend),
}

impl State {
    fn published(&mut self) -> &mut Pend {
        match self {
            Self::Fresh => unreachable!("roles are issued only after publication"),
            Self::Published(pend) => pend,
        }
    }

    fn resolve(&mut self) -> Result<AllocationPermit, Error> {
        let pend = self.published();
        let permit = pend.holding.ok_or(Error::Exhausted)?;
        match pend.branch {
            Admission::Open => Ok(permit),
            Admission::Shut => Err(Error::Revoked),
        }
    }
}

/// The permanent kernel space's preallocated initial receive position. The
/// sole local handle is never reused. The initial branch has no parent,
/// descendants or persistent uses. Its gate and holding share this Turn.
pub struct PendSpace {
    state: TurnLock<State>,
}

/// Preallocated receiving fields of the actual spending and close owners.
pub struct Initial<'receiver> {
    pub spending: &'receiver mut Option<Handle>,
    pub closing: &'receiver mut Option<Revoker>,
}

impl PendSpace {
    pub const fn new() -> Self {
        Self {
            state: TurnLock::new(State::Fresh),
        }
    }

    /// Install the first root and its two distinct roles in one commit.
    ///
    /// # Safety
    ///
    /// The trusted boot owner must still have initial issuance authority
    /// for this Allowance and the exact kernel receiver. The PendSpace is
    /// already in permanent, supplied storage and is the original receiver.
    /// Both output fields must be the durable receivers of their own roles.
    pub unsafe fn publish_initial(
        &'static self,
        allowance: &Allowance,
        maximum: ByteLength,
        alignment: Alignment,
        roles: Initial<'_>,
        mode: TurnMode,
    ) -> Result<(), Error> {
        if maximum.get() == 0 {
            return Err(Error::Limit);
        }
        if roles.spending.is_some() || roles.closing.is_some() {
            return Err(Error::Receiver);
        }
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(&self.state).expect("initial PendSpace");
        allowance.plan_spender(&mut plan)?;
        plan.acquire(mode, |held| {
            allowance.check_spender(held)?;
            let receiver = held.access(&self.state).expect("initial PendSpace");
            if let State::Published(pend) = receiver {
                return Err(if pend.holding.is_some() {
                    Error::Installed
                } else {
                    Error::Exhausted
                });
            }
            *receiver = State::Published(Pend {
                holding: Some(AllocationPermit {
                    source: allowance.source(),
                    allowance: allowance.id(),
                    maximum,
                    alignment,
                }),
                branch: Admission::Open,
            });
            *roles.spending = Some(Handle { space: self });
            *roles.closing = Some(Revoker { space: self });
            Ok(())
        })
        .map_err(allocator::Error::from)?
    }
}

impl Default for PendSpace {
    fn default() -> Self {
        Self::new()
    }
}

/// Close authority for this exact root, independently retained after Release.
pub struct Revoker {
    space: &'static PendSpace,
}

impl Revoker {
    pub fn revoke(&self) -> Result<(), Error> {
        Ok(self.space.state.acquire(|state| {
            state.published().branch = Admission::Shut;
        })?)
    }

    pub fn revoked(&self) -> Result<bool, Error> {
        Ok(self
            .space
            .state
            .acquire(|state| matches!(state.published().branch, Admission::Shut))?)
    }
}

/// A local address in one exact, permanent PendSpace. Copies can refer only
/// to the same holding. They do not establish additional external holdings.
#[derive(Clone, Copy)]
pub struct Handle {
    space: &'static PendSpace,
}

impl Handle {
    fn spender(self) -> Spender {
        Spender(ptr::from_ref(&self.space.state).addr())
    }

    fn resolve(self, held: &mut TurnAccess<'_>) -> Result<AllocationPermit, Error> {
        held.access(&self.space.state)
            .ok_or(allocator::Error::Protection)?
            .resolve()
    }

    pub fn reserve(
        self,
        candidate: Candidate,
        receiver: &'static Construction,
        mode: TurnMode,
    ) -> Result<AllocationId, Error> {
        let permit = self.space.state.acquire(|state| state.resolve())??;
        let mut work = TurnWorkspace::<4>::new();
        let mut plan = work.plan();
        plan.insert(&self.space.state).expect("original PendSpace");
        permit.source.plan_construction(
            &mut plan,
            candidate.region(),
            permit.allowance,
            receiver,
        )?;
        plan.acquire(mode, |held| {
            let permit = self.resolve(held)?;
            permit.check(candidate)?;
            // The original holding, its root branch, source and receiver are
            // all protected by this plan. Nothing from discovery authorizes.
            Ok(unsafe {
                permit.source.reserve(
                    held,
                    candidate,
                    permit.allowance,
                    receiver,
                    Some(self.spender()),
                )
            }?)
        })
        .map_err(allocator::Error::from)?
    }

    pub fn issue(
        self,
        receiver: &'static Construction,
        mode: TurnMode,
    ) -> Result<AllocationId, Error> {
        let permit = self.space.state.acquire(|state| state.resolve())??;
        let (region, allowance) = receiver.reservation()?;
        if allowance != permit.allowance {
            return Err(allocator::Error::Protection.into());
        }
        let mut work = TurnWorkspace::<4>::new();
        let mut plan = work.plan();
        plan.insert(&self.space.state).expect("original PendSpace");
        permit
            .source
            .plan_construction(&mut plan, region, allowance, receiver)?;
        plan.acquire(mode, |held| {
            let permit = self.resolve(held)?;
            let candidate = permit.source.reserved_candidate(held, receiver)?;
            permit.check(candidate)?;
            // The allocator matches the original spending position before
            // issuing. A different Pend over the same share cannot substitute.
            Ok(unsafe { permit.source.issue(held, receiver, Some(self.spender())) }?)
        })
        .map_err(allocator::Error::from)?
    }

    /// End the one external holding. Existing Allocator obligations remain
    /// in their original construction and source records.
    pub fn release(self) -> Result<(), Error> {
        self.space.state.acquire(|state| {
            state.published().holding.take().ok_or(Error::Exhausted)?;
            Ok(())
        })?
    }
}

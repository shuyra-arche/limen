//! Root allocation authority installed in the kernel's initial PendSpace.

use core::ptr;

use crate::{
    allocator::{
        self, AllocationId, Allocator, Allowance, AllowanceId, Candidate, Construction, RamPurpose,
        Spender, with_turn,
    },
    sync::{Acquisition, HeldLockSet, TurnLock, Work},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Limit,
    Empty,
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

/// The actual source and share, bounded to metadata requests. No Cell or
/// initial Cell handle can be issued from this permit.
#[derive(Clone, Copy)]
struct AllocationPermit {
    source: &'static Allocator,
    allowance: AllowanceId,
    maximum: u64,
    alignment: u64,
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
    permit: AllocationPermit,
    branch: Admission,
}

enum Holding {
    Empty,
    Held(Pend),
    Exhausted,
}

impl Holding {
    fn resolve(&self) -> Result<AllocationPermit, Error> {
        match self {
            Self::Empty => Err(Error::Empty),
            Self::Exhausted => Err(Error::Exhausted),
            Self::Held(Pend {
                branch: Admission::Shut,
                ..
            }) => Err(Error::Revoked),
            Self::Held(pend) => Ok(pend.permit),
        }
    }
}

/// The permanent kernel space's preallocated initial receive position. The
/// sole local handle is never reused. The initial branch has no parent,
/// descendants or persistent uses. Its gate and holding share this Turn.
pub struct PendSpace {
    state: TurnLock<Holding>,
}

impl PendSpace {
    pub const fn new() -> Self {
        Self {
            state: TurnLock::new(Holding::Empty),
        }
    }

    /// Install the first root and its sole external holding together.
    ///
    /// # Safety
    ///
    /// The trusted boot owner must still have initial issuance authority
    /// for this Allowance and the exact kernel receiver. The PendSpace is
    /// already in permanent, supplied storage and is the original receiver.
    /// Only the trusted owner can distribute its initial local handle.
    pub unsafe fn publish_initial(
        &'static self,
        allowance: &Allowance,
        maximum: u64,
        alignment: u64,
        mode: Acquisition,
    ) -> Result<(), Error> {
        if maximum == 0 || !alignment.is_power_of_two() {
            return Err(Error::Limit);
        }
        let mut work = Work::<2>::new();
        let mut plan = work.plan();
        plan.insert(&self.state).expect("initial PendSpace");
        allowance.plan_spender(&mut plan)?;
        plan.execute(mode, |held| {
            allowance.check_spender(held)?;
            let receiver = held.access(&self.state).expect("initial PendSpace");
            match receiver {
                Holding::Empty => {}
                Holding::Held(_) => return Err(Error::Installed),
                Holding::Exhausted => return Err(Error::Exhausted),
            }
            *receiver = Holding::Held(Pend {
                permit: AllocationPermit {
                    source: allowance.source(),
                    allowance: allowance.id(),
                    maximum,
                    alignment,
                },
                branch: Admission::Open,
            });
            Ok(())
        })
        .map_err(allocator::Error::from)?
    }

    /// The original space can rediscover the local handle after publication.
    /// It remains discoverable for release after the branch shuts.
    pub fn initial(&'static self) -> Result<Handle, Error> {
        with_turn(&self.state, |state| match state {
            Holding::Empty => Err(Error::Empty),
            Holding::Held(_) => Ok(Handle { space: self }),
            Holding::Exhausted => Err(Error::Exhausted),
        })?
    }

    /// The trusted original branch owner closes future admission.
    pub fn revoke(&self) -> Result<(), Error> {
        with_turn(&self.state, |state| match state {
            Holding::Held(pend) => {
                pend.branch = Admission::Shut;
                Ok(())
            }
            Holding::Empty => Err(Error::Empty),
            Holding::Exhausted => Err(Error::Exhausted),
        })?
    }
}

impl Default for PendSpace {
    fn default() -> Self {
        Self::new()
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

    fn resolve(self, held: &mut HeldLockSet<'_>) -> Result<AllocationPermit, Error> {
        held.access(&self.space.state)
            .ok_or(allocator::Error::Protection)?
            .resolve()
    }

    pub fn reserve(
        self,
        candidate: Candidate,
        receiver: &'static TurnLock<Construction>,
        mode: Acquisition,
    ) -> Result<AllocationId, Error> {
        let permit = with_turn(&self.space.state, |state| state.resolve())??;
        let mut work = Work::<4>::new();
        let mut plan = work.plan();
        plan.insert(&self.space.state).expect("original PendSpace");
        permit.source.plan_construction(
            &mut plan,
            candidate.region(),
            permit.allowance,
            receiver,
        )?;
        plan.execute(mode, |held| {
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
        receiver: &'static TurnLock<Construction>,
        mode: Acquisition,
    ) -> Result<AllocationId, Error> {
        let permit = with_turn(&self.space.state, |state| state.resolve())??;
        let (region, allowance) =
            with_turn(receiver, |state| state.reservation())?.ok_or(allocator::Error::Receiver)?;
        if allowance != permit.allowance {
            return Err(allocator::Error::Protection.into());
        }
        let mut work = Work::<4>::new();
        let mut plan = work.plan();
        plan.insert(&self.space.state).expect("original PendSpace");
        permit
            .source
            .plan_construction(&mut plan, region, allowance, receiver)?;
        plan.execute(mode, |held| {
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
        with_turn(&self.space.state, |state| match state {
            Holding::Held(_) => {
                *state = Holding::Exhausted;
                Ok(())
            }
            Holding::Empty => Err(Error::Empty),
            Holding::Exhausted => Err(Error::Exhausted),
        })?
    }
}

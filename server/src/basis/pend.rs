//! Root allocation authority installed in the kernel's initial PendSpace.

use limen_treaty::model::{Alignment, ByteLength};

use crate::metal::{
    memory::allocator::treaty::{
        self as allocator, AllocationConstruction, AllocationId, AllocationPermit,
        AllocationPermitSlot, Allowance, Candidate, InitialAccess, LayoutLimit, MappedStorage,
        RamPurpose,
    },
    sync::treaty::{TurnAccess, TurnAcquireError, TurnLock, TurnMode, TurnPlan, TurnWorkspace},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Receiver,
    Installed,
    Revoked,
    Exhausted,
    Protection,
    Source(allocator::Error),
    Acquire(TurnAcquireError),
}

impl From<allocator::Error> for Error {
    fn from(error: allocator::Error) -> Self {
        Self::Source(error)
    }
}

impl From<TurnAcquireError> for Error {
    fn from(error: TurnAcquireError) -> Self {
        Self::Acquire(error)
    }
}

enum Admission {
    Open,
    Shut,
}

pub(crate) struct Pend<Permit> {
    holding: Option<Permit>,
    branch: Admission,
}

impl<Permit: Copy> Pend<Permit> {
    pub(crate) fn held(permit: Permit) -> Self {
        Self {
            holding: Some(permit),
            branch: Admission::Open,
        }
    }

    pub(crate) fn resolve(&self) -> Result<Permit, Error> {
        let permit = self.holding.ok_or(Error::Exhausted)?;
        match self.branch {
            Admission::Open => Ok(permit),
            Admission::Shut => Err(Error::Revoked),
        }
    }

    pub(crate) fn consume(&mut self) -> Result<Permit, Error> {
        self.holding.take().ok_or(Error::Exhausted)
    }

    pub(crate) fn shut(&mut self) {
        self.branch = Admission::Shut;
    }
}

enum State {
    Fresh,
    Published(Pend<AllocationPermit>),
}

impl State {
    fn published(&mut self) -> &mut Pend<AllocationPermit> {
        match self {
            Self::Fresh => unreachable!("roles are issued only after publication"),
            Self::Published(pend) => pend,
        }
    }

    fn resolve(&mut self) -> Result<AllocationPermit, Error> {
        self.published().resolve()
    }
}

/// The permanent kernel space's preallocated initial receive position. The
/// sole local handle is never reused. The initial branch has no parent,
/// descendants or persistent uses. Its gate and holding share this Turn.
pub struct PendSpace {
    permit_slot: AllocationPermitSlot,
    state: TurnLock<State>,
}

/// Preallocated receiving fields of the actual spending and close owners.
pub struct Initial<'receiver> {
    pub spending: &'receiver mut Option<Handle>,
    pub closing: &'receiver mut Option<Revoker>,
}

impl PendSpace {
    pub const fn new() -> Self {
        Self::with_permit(AllocationPermitSlot::metadata())
    }

    /// Supply the permanent initial spending position for table pages.
    /// The trusted boot issuer supplies the actual source and layout bounds.
    pub const fn for_page_tables() -> Self {
        Self::with_permit(AllocationPermitSlot::page_tables())
    }

    /// Supply the permanent initial spending position for ordinary RAM Cells.
    /// The original issuer bounds the first publication to Read/Write; this
    /// spending position does not itself grant access to a published Cell.
    pub const fn for_cells() -> Self {
        Self::with_permit(AllocationPermitSlot::cells())
    }

    const fn with_permit(permit_slot: AllocationPermitSlot) -> Self {
        Self {
            permit_slot,
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
        let limit = LayoutLimit::new(maximum, alignment)?;
        if roles.spending.is_some() || roles.closing.is_some() {
            return Err(Error::Receiver);
        }
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(&self.state).expect("initial PendSpace");
        allowance.plan_initial_permit(&mut plan)?;
        plan.acquire(mode, |held| {
            let permit = allowance.issue_initial_permit(held, &self.permit_slot, limit)?;
            let receiver = held.access(&self.state).expect("initial PendSpace");
            if let State::Published(pend) = receiver {
                return Err(if pend.holding.is_some() {
                    Error::Installed
                } else {
                    Error::Exhausted
                });
            }
            *receiver = State::Published(Pend::held(permit));
            *roles.spending = Some(Handle { space: self });
            *roles.closing = Some(Revoker { space: self });
            Ok(())
        })?
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
            state.published().shut();
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
    fn resolve(self, held: &mut TurnAccess<'_>) -> Result<AllocationPermit, Error> {
        held.access(&self.space.state)
            .ok_or(Error::Protection)?
            .resolve()
    }

    pub(crate) fn plan_initial_access(self, plan: &mut TurnPlan<'_, '_>) -> Result<(), Error> {
        let permit = self.space.state.acquire(|state| state.resolve())??;
        plan.insert(&self.space.state)
            .expect("original spending position");
        permit.plan_allowance(plan)?;
        Ok(())
    }

    pub(crate) fn initial_access(
        self,
        held: &mut TurnAccess<'_>,
        storage: &MappedStorage<'_>,
        purpose: RamPurpose,
    ) -> Result<InitialAccess, Error> {
        let permit = self.resolve(held)?;
        let initial = storage.initial_access(permit)?;
        if permit.purpose() != purpose {
            return Err(allocator::Error::PermitLimit.into());
        }
        permit.check_allowance(held)?;
        Ok(initial)
    }

    pub fn reserve(
        self,
        candidate: Candidate,
        receiver: &'static AllocationConstruction,
        mode: TurnMode,
    ) -> Result<AllocationId, Error> {
        let permit = self.space.state.acquire(|state| state.resolve())??;
        let mut work = TurnWorkspace::<4>::new();
        let mut plan = work.plan();
        plan.insert(&self.space.state).expect("original PendSpace");
        permit.source().plan_construction(
            &mut plan,
            candidate.region(),
            permit.allowance(),
            receiver,
        )?;
        plan.acquire(mode, |held| {
            let permit = self.resolve(held)?;
            // The original holding, its root branch, source and receiver are
            // all protected by this plan. Nothing from discovery authorizes.
            Ok(unsafe { permit.source().reserve(held, candidate, permit, receiver) }?)
        })?
    }

    pub fn issue(
        self,
        receiver: &'static AllocationConstruction,
        mode: TurnMode,
    ) -> Result<AllocationId, Error> {
        let permit = self.space.state.acquire(|state| state.resolve())??;
        let (region, allowance) = receiver.reservation()?;
        if allowance != permit.allowance() {
            return Err(allocator::Error::PermitOrigin.into());
        }
        let mut work = TurnWorkspace::<4>::new();
        let mut plan = work.plan();
        plan.insert(&self.space.state).expect("original PendSpace");
        permit
            .source()
            .plan_construction(&mut plan, region, allowance, receiver)?;
        plan.acquire(mode, |held| {
            let permit = self.resolve(held)?;
            // The allocator matches the original spending position before
            // issuing. A different Pend over the same share cannot substitute.
            Ok(unsafe { permit.source().issue(held, receiver, permit) }?)
        })?
    }

    /// End the one external holding. Existing Allocator obligations remain
    /// in their original construction and source records.
    pub fn release(self) -> Result<(), Error> {
        self.space.state.acquire(|state| {
            state.published().consume()?;
            Ok(())
        })?
    }
}

//! A retained local Internal Nest and the recoverable execution references.
//! The trusted Kernel owner holds the actual installed root. No ordinary
//! DELETE authority, Span, Mapping or remote residency is inferred here.

use core::ptr;

use crate::metal::{
    identity::treaty::Generation,
    memory::mmu::treaty::{Context, LocalRoot, TableError, TableId},
    sync::treaty::{TurnAcquireError, TurnLock, TurnMode},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NestId {
    scope: usize,
    generation: Generation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Open,
    Shutting,
    Draining,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Observation {
    pub root: TableId,
    pub phase: Phase,
    pub recoverable_refs: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Acquire(TurnAcquireError),
    Table(TableError),
    Busy,
    Gone,
    Shut,
    Scope,
    State,
    Exhausted,
}

impl From<TurnAcquireError> for Error {
    fn from(error: TurnAcquireError) -> Self {
        Self::Acquire(error)
    }
}

/// The original stable Nest instance position. Its identity is retained
/// during Draining, including after local withdrawal, until the actual
/// whole-tree retirement and refund contract can remove the instance.
pub struct Nest {
    state: TurnLock<State>,
}

pub(super) struct State {
    generation: Option<Generation>,
    record: Option<Record>,
}

struct Record {
    id: NestId,
    root: TableId,
    context: Context,
    phase: Phase,
    recoverable_refs: usize,
}

impl State {
    fn record(&mut self, id: NestId) -> Result<&mut Record, Error> {
        self.record.as_mut().filter(|record| record.id == id).ok_or(Error::Gone)
    }

    pub(super) fn retain(&mut self, id: NestId, processor: usize) -> Result<NestRef, Error> {
        let record = self.record(id)?;
        if record.phase != Phase::Open {
            return Err(Error::Shut);
        }
        if record.context.processor() != processor || !record.context.is_current() {
            return Err(Error::Scope);
        }
        record.recoverable_refs = record.recoverable_refs.checked_add(1).ok_or(Error::Exhausted)?;
        Ok(NestRef(id))
    }

    pub(super) fn release(&mut self, id: NestId) -> Result<(), Error> {
        let record = self.record(id)?;
        record.recoverable_refs = record.recoverable_refs.checked_sub(1).ok_or(Error::State)?;
        Ok(())
    }
}

/// A non-cloneable execution-lifetime relation. Only Unit publication can
/// commit it; the original Ended commit explicitly consumes its count.
pub(super) struct NestRef(NestId);

impl NestRef {
    pub(super) fn id(&self) -> NestId {
        self.0
    }
}

/// Exact binding of an Execution to its trusted Kernel Nest. This identity
/// alone does not extend the Nest lifetime or permit another Unit.
#[derive(Clone, Copy)]
pub(super) struct Binding {
    nest: &'static Nest,
    id: NestId,
}

impl Binding {
    pub(super) fn id(self) -> NestId {
        self.id
    }

    pub(super) fn state(self) -> &'static TurnLock<State> {
        &self.nest.state
    }

    pub(super) fn check_open(self) -> Result<(), Error> {
        self.state().acquire(|state| {
            let record = state.record(self.id)?;
            if record.phase != Phase::Open {
                return Err(Error::Shut);
            }
            if !record.context.is_current() {
                return Err(Error::Scope);
            }
            Ok(())
        })?
    }

    pub(super) fn check_current(self) -> Result<(), Error> {
        self.state().acquire(|state| {
            let record = state.record(self.id)?;
            if record.phase == Phase::Draining {
                return Err(Error::Shut);
            }
            if !record.context.is_current() {
                return Err(Error::Scope);
            }
            Ok(())
        })?
    }
}

impl Nest {
    pub const fn new() -> Self {
        Self {
            state: TurnLock::new(State {
                generation: None,
                record: None,
            }),
        }
    }

    /// Accept an already installed original owned root. No copied physical
    /// address or merely prepared tree can establish this relation.
    pub fn publish<'control, 'scope>(
        &'static self,
        root: LocalRoot<'control, 'scope>,
    ) -> Result<LocalNest<'control, 'scope>, (LocalRoot<'control, 'scope>, Error)> {
        let Some((table, context)) = root.installed() else {
            return Err((root, Error::State));
        };
        let published = self.state.acquire(|state| {
            if state.record.is_some() {
                return Err(Error::Busy);
            }
            let generation = Generation::next(state.generation).ok_or(Error::Exhausted)?;
            let id = NestId { scope: ptr::from_ref(self).addr(), generation };
            state.generation = Some(generation);
            state.record = Some(Record {
                id,
                root: table,
                context,
                phase: Phase::Open,
                recoverable_refs: 0,
            });
            Ok(id)
        }).map_err(Error::from).and_then(core::convert::identity);
        match published {
            Ok(id) => Ok(LocalNest { binding: Binding { nest: self, id }, root }),
            Err(error) => Err((root, error)),
        }
    }

    pub fn observe(&self, id: NestId) -> Result<Observation, Error> {
        self.state.acquire(|state| {
            let record = state.record(id)?;
            Ok(Observation {
                root: record.root,
                phase: record.phase,
                recoverable_refs: record.recoverable_refs,
            })
        })?
    }
}

impl Default for Nest {
    fn default() -> Self {
        Self::new()
    }
}

/// The trusted Kernel Nest's actual local owner. It retains the unique
/// root installation, including any original inherited branch. Losing the
/// handle does not reopen its table pages or erase the stable record.
#[must_use]
pub struct LocalNest<'control, 'scope> {
    binding: Binding,
    root: LocalRoot<'control, 'scope>,
}

impl LocalNest<'_, '_> {
    pub fn id(&self) -> NestId {
        self.binding.id
    }

    pub(super) fn binding(&self) -> Binding {
        self.binding
    }

    /// Trusted platform closure; ordinary Nest DELETE capability is a
    /// separate admission contract. Existing references remain valid.
    pub fn begin_delete(&mut self) -> Result<(), Error> {
        self.binding.state().acquire(|state| {
            let record = state.record(self.id())?;
            match record.phase {
                Phase::Open => record.phase = Phase::Shutting,
                Phase::Shutting => {}
                Phase::Draining => return Err(Error::State),
            }
            Ok(())
        })?
    }

    pub fn begin_drain(&mut self) -> Result<(), Error> {
        self.binding.state().acquire(|state| {
            let record = state.record(self.id())?;
            if record.phase == Phase::Draining {
                return Ok(());
            }
            if record.phase != Phase::Shutting {
                return Err(Error::State);
            }
            if record.recoverable_refs != 0 {
                return Err(Error::Busy);
            }
            record.phase = Phase::Draining;
            Ok(())
        })?
    }

    /// Withdraw the exact native root and detach its retained installation.
    /// Keep the instance in Draining: local withdrawal cannot prove that
    /// all original table-page Return duties have settled.
    ///
    /// # Safety
    /// The trusted platform has ended all independent uses of this root
    /// apart from the actual restore path. All working mappings and
    /// retirement supplies remain live; the original control is exclusive.
    pub unsafe fn withdraw(&mut self) -> Result<(), Error> {
        self.binding.state().acquire(|state| {
            let record = state.record(self.id())?;
            if record.phase != Phase::Draining || record.recoverable_refs != 0 {
                return Err(Error::Busy);
            }
            Ok(())
        })??;
        // Never retain the Nest Turn across native changes or nested table
        // ownership. Draining excludes every new recoverable reference.
        unsafe { self.root.withdraw(TurnMode::Slow) }.map_err(Error::Table)
    }
}

//! A retained local Internal Nest and the recoverable execution references.
//! The trusted Kernel owner holds the actual installed root. No ordinary
//! DELETE authority, general Mapping or remote residency is inferred here.

use core::ptr;

use limen_treaty::model::{ByteLength, InternalAddress, InternalRange};

mod mapping;
mod span;
pub use mapping::{MappingId, MappingObservation, MappingPhase, MappingProgress, MappingStorage};
use mapping::{MappingIndex, Work};
use span::SpanIndex;
pub use span::{SpanId, SpanObservation, SpanPhase, SpanStorage};

use crate::metal::{
    identity::treaty::Generation,
    memory::mmu::treaty::{Context, LocalRoot, TableError, TableId, TableProgress, TableStore},
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
pub enum Progress {
    Pending,
    Retired,
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
    Cell(super::cell::Error),
    Busy,
    Capacity,
    Conflict,
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

/// Stable storage for the original Nest. Its identity remains discoverable
/// during Draining until the original whole-tree returns are acknowledged.
pub struct Nest {
    state: TurnLock<State>,
}

pub(super) struct State {
    generation: Option<Generation>,
    record: Option<Record>,
    spans: SpanIndex,
    mappings: MappingIndex,
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
        self.record
            .as_mut()
            .filter(|record| record.id == id)
            .ok_or(Error::Gone)
    }

    pub(super) fn retain(&mut self, id: NestId, processor: usize) -> Result<NestRef, Error> {
        let record = self.record(id)?;
        if record.phase != Phase::Open {
            return Err(Error::Shut);
        }
        if record.context.processor() != processor || !record.context.is_current() {
            return Err(Error::Scope);
        }
        record.recoverable_refs = record
            .recoverable_refs
            .checked_add(1)
            .ok_or(Error::Exhausted)?;
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
    pub fn new<const SPANS: usize, const MAPPINGS: usize>(
        spans: &'static mut SpanStorage<SPANS>,
        mappings: &'static mut MappingStorage<MAPPINGS>,
    ) -> Result<Self, Error> {
        Ok(Self {
            state: TurnLock::new(State {
                generation: None,
                record: None,
                spans: SpanIndex::new(spans)?,
                mappings: MappingIndex::new(mappings)?,
            }),
        })
    }

    /// Accept an already installed original owned root. No copied physical
    /// address or merely prepared tree can establish this relation.
    pub fn publish<'control, 'scope>(
        &'static self,
        root: LocalRoot<'control, 'scope>,
    ) -> Result<LocalNest<'control, 'scope>, (LocalRoot<'control, 'scope>, Error)> {
        if root.has_translations() || root.has_pending_supply() {
            return Err((root, Error::Busy));
        }
        let Some((table, context)) = root.installed() else {
            return Err((root, Error::State));
        };
        let published = self
            .state
            .acquire(|state| {
                if state.record.is_some() || !state.spans.is_empty() || !state.mappings.is_empty() {
                    return Err(Error::Busy);
                }
                let generation = Generation::next(state.generation).ok_or(Error::Exhausted)?;
                let id = NestId {
                    scope: ptr::from_ref(self).addr(),
                    generation,
                };
                state.generation = Some(generation);
                state.record = Some(Record {
                    id,
                    root: table,
                    context,
                    phase: Phase::Open,
                    recoverable_refs: 0,
                });
                Ok(id)
            })
            .map_err(Error::from)
            .and_then(core::convert::identity);
        match published {
            Ok(id) => Ok(LocalNest {
                binding: Binding { nest: self, id },
                root,
                work: Work::new(),
                retired: false,
            }),
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

/// The trusted Kernel Nest's actual local owner. It retains the unique
/// root installation, including any original inherited branch. Losing the
/// handle does not reopen its table pages or erase the stable record.
#[must_use]
pub struct LocalNest<'control, 'scope> {
    binding: Binding,
    root: LocalRoot<'control, 'scope>,
    work: Work,
    retired: bool,
}

impl LocalNest<'_, '_> {
    pub fn id(&self) -> NestId {
        self.binding.id
    }

    pub(super) fn binding(&self) -> Binding {
        self.binding
    }

    /// Reserve a page-aligned ordinary interval in the exact owned local
    /// root's original unoccupied layout. This trusted owner does not imply
    /// a public NestPermit or a Recoverable Span.
    pub fn reserve_span(&mut self, range: InternalRange) -> Result<SpanId, Error> {
        if !range.start().get().is_multiple_of(4096) || !range.end().get().is_multiple_of(4096) {
            return Err(Error::Scope);
        }
        self.binding.state().acquire(|state| {
            let record = state.record(self.id())?;
            if record.phase != Phase::Open {
                return Err(Error::Shut);
            }
            Ok(())
        })??;
        // The exclusive LocalRoot keeps this layout check valid across the
        // Nest Turn acquisition. Never acquire table Turns under the Nest.
        if !self
            .root
            .admits_span(range, TurnMode::Slow)
            .map_err(Error::Table)?
        {
            return Err(Error::Scope);
        }
        self.binding.state().acquire(|state| {
            let record = state.record(self.id())?;
            if record.phase != Phase::Open {
                return Err(Error::Shut);
            }
            state.spans.reserve(self.id(), range)
        })?
    }

    fn check_supply(&self, span: SpanId, address: InternalAddress) -> Result<(), Error> {
        if self.work.prepared.is_some() || self.work.active.is_some() {
            return Err(Error::Busy);
        }
        let end = address.advance(ByteLength::new(4096)).ok_or(Error::Scope)?;
        let range = InternalRange::new(address, end).ok_or(Error::Scope)?;
        if !address.get().is_multiple_of(4096) {
            return Err(Error::Scope);
        }
        self.binding.state().acquire(|state| {
            if state.record(self.id())?.phase == Phase::Draining {
                return Err(Error::Shut);
            }
            state.spans.check_map(span, range)
        })?
    }

    /// Locate the next real missing table for this open Span.
    pub fn missing_table(
        &self,
        span: SpanId,
        address: InternalAddress,
        mode: TurnMode,
    ) -> Result<Option<usize>, Error> {
        self.check_supply(span, address)?;
        self.root.missing_table(address, mode).map_err(Error::Table)
    }

    /// Supply a complete already linked private empty-leaf chain.
    /// Ok transfers the whole original subtree before native Make.
    ///
    /// # Safety
    /// The caller exclusively owns the prepared chain. Exclude independent
    /// private editors and retirement through validation and acceptance.
    /// Preserve all original coherent table RAM and working mappings
    /// through the root's retirement, under the admitted local control.
    pub unsafe fn supply_path(
        &mut self,
        span: SpanId,
        address: InternalAddress,
        child: &'static TableStore,
        id: TableId,
        mode: TurnMode,
    ) -> Result<(), Error> {
        self.check_supply(span, address)?;
        // The trusted LocalNest is the exclusive Span/translation editor.
        // No Nest Turn is held across the actual table/Cell commit.
        unsafe { self.root.supply_path(address, child, id, mode) }.map_err(Error::Table)
    }

    /// Complete an already accepted table supply even if its Span closed.
    ///
    /// # Safety
    /// Retain the original local context and all table material.
    pub unsafe fn advance_supply(&mut self, mode: TurnMode) -> Result<(), Error> {
        unsafe { self.root.advance_supply(mode) }.map_err(Error::Table)
    }

    pub fn observe_span(&self, id: SpanId) -> Result<SpanObservation, Error> {
        self.binding.state().acquire(|state| {
            state.record(self.id())?;
            state.spans.observe(id)
        })?
    }

    pub fn begin_release_span(&mut self, id: SpanId) -> Result<(), Error> {
        self.binding.state().acquire(|state| {
            state.record(self.id())?;
            state.spans.close(id)
        })?
    }

    pub fn finish_release_span(&mut self, id: SpanId) -> Result<(), Error> {
        self.binding.state().acquire(|state| {
            state.record(self.id())?;
            let range = state.spans.observe(id)?.range;
            if state.mappings.overlaps(range)
                || self.work.prepared.is_some()
                || self.root.has_pending_supply()
            {
                return Err(Error::Busy);
            }
            state.spans.settle(id)
        })?
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

    /// Advance a bounded original retirement step. A local root withdrawal
    /// does not remove the instance. Only the original root owner's complete
    /// tree settlement can authorize removing it from stable storage.
    ///
    /// # Safety
    /// The trusted platform has ended every independent use of this root
    /// except the actual restore path. Keep both working mappings and the
    /// original exclusive control live until native withdrawal is settled.
    /// The receiving LocalNest remains resident through all outstanding
    /// table-page Return duties. No other owner may use its private tables.
    pub unsafe fn advance_retirement(&mut self) -> Result<Progress, Error> {
        if self.retired {
            return Ok(Progress::Retired);
        }
        self.binding.state().acquire(|state| {
            let record = state.record(self.id())?;
            if record.phase != Phase::Draining || record.recoverable_refs != 0 {
                return Err(Error::Busy);
            }
            Ok(())
        })??;
        if self.root.has_pending_supply() {
            unsafe { self.advance_supply(TurnMode::Slow) }?;
            return Ok(Progress::Pending);
        }
        if self.work.prepared.is_some() {
            self.cancel_preparation(TurnMode::Slow)?;
            return Ok(Progress::Pending);
        }
        let mapping = self.binding.state().acquire(|state| {
            state.record(self.id())?;
            Ok::<_, Error>(state.mappings.first())
        })??;
        if let Some(mapping) = self.work.active.as_ref().map(|work| work.id).or(mapping) {
            self.begin_unmap(mapping)?;
            unsafe { self.advance_mapping(mapping, TurnMode::Slow) }?;
            return Ok(Progress::Pending);
        }
        let pending_span = self.binding.state().acquire(|state| {
            let record = state.record(self.id())?;
            if record.phase != Phase::Draining || record.recoverable_refs != 0 {
                return Err(Error::Busy);
            }
            Ok(state.spans.advance())
        })??;
        if pending_span {
            return Ok(Progress::Pending);
        }
        // Draining closes new admission. Never retain the Nest Turn across
        // native root installation or an independently protected refund.
        if unsafe { self.root.advance_retirement(TurnMode::Slow) }.map_err(Error::Table)?
            == TableProgress::Pending
        {
            return Ok(Progress::Pending);
        }
        self.binding.state().acquire(|state| {
            let record = state.record(self.id())?;
            assert_eq!(record.phase, Phase::Draining);
            assert_eq!(record.recoverable_refs, 0);
            assert!(state.spans.is_empty());
            assert!(state.mappings.is_empty());
            state.record = None;
            Ok::<(), Error>(())
        })??;
        self.retired = true;
        Ok(Progress::Retired)
    }
}

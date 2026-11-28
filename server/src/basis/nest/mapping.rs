//! One ordinary Mapping owns the accepted use and its actual native effect.

use core::{cmp::Ordering, ptr};

use limen_treaty::model::{InternalAddress, InternalRange};

use crate::{
    basis::cell::{AcceptedAccess, AccessId, AccessPreparation, MappingInput, Rights},
    metal::{
        identity::treaty::{Entry, GenerationTable, Key, ReserveError},
        index::{Insertion, Link, Tree},
        memory::mmu::treaty::{LocalTranslation, TranslationPhase},
        sync::treaty::{TurnAcquireError, TurnMode, TurnWorkspace},
    },
};

use super::{Error, LocalNest, NestId, Phase, SpanId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappingId(Key<NestId>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingPhase {
    Installing,
    Active,
    Removing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingProgress {
    Active,
    Pending,
    Removed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappingObservation {
    pub span: SpanId,
    pub range: InternalRange,
    pub rights: Rights,
    pub access: AccessId,
    pub phase: MappingPhase,
}

struct Mapping {
    id: MappingId,
    span: SpanId,
    range: InternalRange,
    rights: Rights,
    access: AcceptedAccess,
    phase: MappingPhase,
    // Taken only by the exclusive local owner into its persistent work
    // position while advancing this exact native effect.
    translation: Option<LocalTranslation>,
}

pub struct MappingStorage<const CAPACITY: usize> {
    entries: [Entry<Mapping>; CAPACITY],
    links: [Link; CAPACITY],
}

impl<const CAPACITY: usize> MappingStorage<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            entries: [const { Entry::EMPTY }; CAPACITY],
            links: [Link::EMPTY; CAPACITY],
        }
    }
}

impl<const CAPACITY: usize> Default for MappingStorage<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) struct MappingIndex {
    scope: usize,
    entries: GenerationTable<'static, usize, Mapping>,
    tree: Tree,
}

fn compare(range: InternalRange, existing: InternalRange) -> Ordering {
    if range.end() <= existing.start() {
        Ordering::Less
    } else if range.start() >= existing.end() {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

impl MappingIndex {
    pub(super) fn new<const CAPACITY: usize>(
        storage: &'static mut MappingStorage<CAPACITY>,
    ) -> Result<Self, Error> {
        if CAPACITY == 0 {
            return Err(Error::Capacity);
        }
        let scope = ptr::from_ref(storage).addr();
        let entries =
            GenerationTable::new(scope, &mut storage.entries).map_err(|_| Error::Capacity)?;
        Ok(Self {
            scope,
            entries,
            tree: Tree::new(&mut storage.links),
        })
    }

    pub(super) fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    pub(super) fn first(&self) -> Option<MappingId> {
        let slot = self.tree.first()?;
        Some(
            self.entries
                .published_at(slot.index())
                .expect("indexed Mapping")
                .1
                .id,
        )
    }

    fn position(&self, range: InternalRange) -> Result<Insertion, Error> {
        self.tree
            .prepare(|slot| {
                let (_, mapping) = self
                    .entries
                    .published_at(slot.index())
                    .expect("original indexed Mapping");
                compare(range, mapping.range)
            })
            .map_err(|_| Error::Conflict)
    }

    pub(super) fn overlaps(&self, range: InternalRange) -> bool {
        self.position(range).is_err()
    }

    fn prepare(&self, nest: NestId, range: InternalRange) -> Result<(Insertion, MappingId), Error> {
        let position = self.position(range)?;
        let key = self.entries.next_key().map_err(|error| match error {
            ReserveError::IdentityExhausted => Error::Exhausted,
            _ => Error::Capacity,
        })?;
        Ok((
            position,
            MappingId(Key::new(nest, key.slot(), key.generation())),
        ))
    }

    fn publish(&mut self, position: Insertion, mapping: Mapping) {
        let id = mapping.id;
        let reservation = self.entries.reserve().expect("preflighted Mapping slot");
        let key = reservation.key();
        assert_eq!(id.0.slot(), key.slot());
        assert_eq!(id.0.generation(), key.generation());
        assert!(self.entries.publish(reservation, mapping).is_ok());
        self.tree.insert(key.slot(), position);
    }

    fn key(&self, id: MappingId) -> Key<usize> {
        Key::new(self.scope, id.0.slot(), id.0.generation())
    }

    fn original(&mut self, id: MappingId) -> Result<&mut Mapping, Error> {
        let key = self.key(id);
        self.entries
            .get_mut(key)
            .filter(|mapping| mapping.id == id)
            .ok_or(Error::Gone)
    }

    fn settle(&mut self, id: MappingId) {
        let key = self.key(id);
        self.tree.remove(key.slot());
        self.entries.detach(key).expect("original indexed Mapping");
        let (reclamation, _) = self.entries.reclaim(key).expect("detached Mapping");
        self.entries.release(reclamation).expect("settled Mapping");
    }
}

pub(super) struct ActiveWork {
    pub(super) id: MappingId,
    translation: LocalTranslation,
    failed_install: bool,
}

/// The exact LocalNest owner is the bounded native step receiver. It does
/// not expose another mapping index or forget a failed preparation.
pub(super) struct Work {
    pub(super) prepared: Option<LocalTranslation>,
    pub(super) active: Option<ActiveWork>,
}

impl Work {
    pub(super) const fn new() -> Self {
        Self {
            prepared: None,
            active: None,
        }
    }
}

impl LocalNest<'_, '_> {
    /// Consume a Prepared access only if the exact Span, Cell authority,
    /// Mapping slot and retained native leaf can jointly admit it.
    /// Failure leaves the original preparation in its original receiver.
    ///
    /// # Safety
    /// The original source's mapped RAM has compatible Normal attributes.
    /// The target interval is not required for the root's execution/stack
    /// or table access continuity; this is the admitted exclusive local
    /// Processor scope, not a remote or device translation.
    pub unsafe fn map(
        &mut self,
        span: SpanId,
        address: InternalAddress,
        receiver: &'static AccessPreparation,
        access: AccessId,
        mode: TurnMode,
    ) -> Result<MappingId, Error> {
        if self.work.prepared.is_some() {
            return Err(Error::Busy);
        }
        let input: MappingInput = receiver.mapping(access).map_err(Error::Cell)?;
        let physical = input.physical();
        let range = address.range(physical.length()).ok_or(Error::Scope)?;
        let binding = self.binding;
        binding.state().acquire(|state| {
            if state.record(binding.id())?.phase == Phase::Draining {
                return Err(Error::Shut);
            }
            state.spans.check_map(span, range)?;
            state.mappings.prepare(binding.id(), range)?;
            Ok(())
        })??;
        let translation = self
            .root
            .prepare_translation(address, physical.start(), physical.length(), input.leaf(), mode)
            .map_err(Error::Table)?;
        self.work.prepared = Some(translation);

        let mut workspace = TurnWorkspace::<4>::new();
        let mut plan = workspace.plan();
        plan.insert(binding.state()).expect("original Nest");
        input.plan(&mut plan);
        let committed = plan
            .acquire(mode, |held| {
                let state = held.access(binding.state()).expect("original Nest");
                if state.record(binding.id())?.phase == Phase::Draining {
                    return Err(Error::Shut);
                }
                state.spans.check_map(span, range)?;
                let (position, id) = state.mappings.prepare(binding.id(), range)?;
                let accepted = input
                    .accept(
                        held,
                        id,
                        range,
                        self.work
                            .prepared
                            .as_ref()
                            .expect("original leaf preparation"),
                    )
                    .map_err(Error::Cell)?;
                let state = held.access(binding.state()).expect("original Nest");
                state.mappings.publish(
                    position,
                    Mapping {
                        id,
                        span,
                        range,
                        rights: input.rights(),
                        access: accepted,
                        phase: MappingPhase::Installing,
                        translation: self.work.prepared.take(),
                    },
                );
                Ok(id)
            })
            .map_err(Error::from)
            .and_then(core::convert::identity);

        if committed.is_err() {
            // A failed cancellation retains the original claim in the local
            // owner. No CellAccess has been accepted; retry cleanup explicitly.
            self.cancel_preparation(mode)?;
        }
        committed
    }

    /// Settle an unaccepted native preparation. It never wrote a leaf.
    pub fn cancel_preparation(&mut self, mode: TurnMode) -> Result<(), Error> {
        if let Some(translation) = self.work.prepared.as_mut() {
            assert!(matches!(
                translation.phase(),
                TranslationPhase::Prepared | TranslationPhase::Removing
            ));
            // At most four original, never-written leaves. A failed step
            // retains the exact remaining claims for a later cancellation.
            while translation.phase() != TranslationPhase::Removed {
                unsafe { self.root.remove_translation(translation, mode) }
                    .map_err(Error::Table)?;
            }
            self.work.prepared = None;
        }
        Ok(())
    }

    pub fn observe_mapping(&self, id: MappingId) -> Result<MappingObservation, Error> {
        self.binding.state().acquire(|state| {
            state.record(self.id())?;
            let original = state.mappings.original(id)?;
            Ok(MappingObservation {
                span: original.span,
                range: original.range,
                rights: original.rights,
                access: original.access.id(),
                phase: original.phase,
            })
        })?
    }

    /// Retirement is a one-way decision. The original address index and
    /// access are retained even if installation never finished.
    pub fn begin_unmap(&mut self, id: MappingId) -> Result<(), Error> {
        self.binding.state().acquire(|state| {
            state.record(self.id())?;
            state.mappings.original(id)?.phase = MappingPhase::Removing;
            Ok(())
        })?
    }

    /// Drive the actual native effect outside Nest/Cell Turns, then settle
    /// its original Mapping. A Busy acknowledgement retains the work.
    ///
    /// # Safety
    /// All independent software users of a removing mapping have stopped.
    /// Retain the original local root, backing RAM and native control scope.
    pub unsafe fn advance_mapping(
        &mut self,
        id: MappingId,
        mode: TurnMode,
    ) -> Result<MappingProgress, Error> {
        let binding = self.binding;
        if self.work.active.as_ref().is_some_and(|work| work.id != id) {
            return Err(Error::Busy);
        }
        let phase = binding.state().acquire(|state| {
            let nest_phase = state.record(binding.id())?.phase;
            let original = state.mappings.original(id)?;
            let (span, range, phase) = (original.span, original.range, original.phase);
            let closing =
                nest_phase == Phase::Draining || state.spans.check_map(span, range).is_err();
            let original = state.mappings.original(id)?;
            if closing
                || self
                    .work
                    .active
                    .as_ref()
                    .is_some_and(|work| work.failed_install)
            {
                original.phase = MappingPhase::Removing;
            }
            if original.phase == MappingPhase::Active {
                assert_eq!(phase, MappingPhase::Active);
                return Ok::<_, Error>(MappingPhase::Active);
            }
            if self.work.active.is_none() {
                self.work.active = Some(ActiveWork {
                    id,
                    translation: original.translation.take().expect("original Translation"),
                    failed_install: false,
                });
            }
            Ok(original.phase)
        })??;
        if phase == MappingPhase::Active {
            return Ok(MappingProgress::Active);
        }
        let work = self.work.active.as_mut().expect("original native work");
        if phase == MappingPhase::Installing {
            if let Err(error) =
                unsafe { self.root.install_translation(&mut work.translation, mode) }
            {
                if error != TableError::Acquire(TurnAcquireError::Busy) {
                    work.failed_install = true;
                }
                return Err(Error::Table(error));
            }
            if work.translation.phase() != TranslationPhase::Active {
                return Ok(MappingProgress::Pending);
            }
            return binding.state().acquire(|state| {
                let original = state.mappings.original(id)?;
                let work = self.work.active.take().expect("installed original work");
                original.translation = Some(work.translation);
                if original.phase == MappingPhase::Installing {
                    original.phase = MappingPhase::Active;
                    Ok(MappingProgress::Active)
                } else {
                    Ok(MappingProgress::Pending)
                }
            })?;
        }

        unsafe { self.root.remove_translation(&mut work.translation, mode) }
            .map_err(Error::Table)?;
        if work.translation.phase() != TranslationPhase::Removed {
            return Ok(MappingProgress::Pending);
        }
        let slot = binding
            .state()
            .acquire(|state| Ok::<_, Error>(state.mappings.original(id)?.access.slot()))??;
        let mut workspace = TurnWorkspace::<2>::new();
        let mut plan = workspace.plan();
        plan.insert(binding.state()).expect("original Nest");
        slot.plan(&mut plan);
        plan.acquire(mode, |held| {
            let state = held.access(binding.state()).expect("original Nest");
            let original = state.mappings.original(id)?;
            if original.phase != MappingPhase::Removing || original.translation.is_some() {
                return Err(Error::State);
            }
            // The actual Cell/Nest protection is shared. The exact accepted
            // use is removed and acknowledged in this same commit.
            let (state, mut cell_locked) = slot.together(held, binding.state());
            let original = state.mappings.original(id)?;
            let work = self.work.active.as_ref().expect("removed original work");
            if work.translation.range() != original.range {
                return Err(Error::Scope);
            }
            original
                .access
                .finish_locked(&mut cell_locked, id, &work.translation)
                .map_err(Error::Cell)?;
            state.mappings.settle(id);
            self.work.active = None;
            Ok(MappingProgress::Removed)
        })?
    }
}

//! Ordinary local Span reservation and settlement, under the owning Nest Turn.

use core::{cmp::Ordering, ptr};

use limen_treaty::model::InternalRange;

use crate::{
    basis::pend::Pend,
    metal::{
        identity::treaty::{Entry, GenerationTable, Key, ReserveError},
        index::{Link, Tree},
    },
};

use super::{Error, NestId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpanId(Key<NestId>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpanPhase {
    Open,
    Shutting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpanObservation {
    pub range: InternalRange,
    pub phase: SpanPhase,
}

struct Span {
    id: SpanId,
    range: InternalRange,
    phase: SpanPhase,
    root: Pend<SpanId>,
}

pub struct SpanStorage<const CAPACITY: usize> {
    entries: [Entry<Span>; CAPACITY],
    links: [Link; CAPACITY],
}

impl<const CAPACITY: usize> SpanStorage<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            entries: [const { Entry::EMPTY }; CAPACITY],
            links: [Link::EMPTY; CAPACITY],
        }
    }
}

impl<const CAPACITY: usize> Default for SpanStorage<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) struct SpanIndex {
    scope: usize,
    entries: GenerationTable<'static, usize, Span>,
    tree: Tree,
}

impl SpanIndex {
    pub(super) fn new<const CAPACITY: usize>(
        storage: &'static mut SpanStorage<CAPACITY>,
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

    pub(super) fn reserve(&mut self, nest: NestId, range: InternalRange) -> Result<SpanId, Error> {
        let position = self
            .tree
            .prepare(|slot| {
                let (_, span) = self
                    .entries
                    .published_at(slot.index())
                    .expect("original Span index member");
                if range.end() <= span.range.start() {
                    Ordering::Less
                } else if range.start() >= span.range.end() {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            })
            .map_err(|_| Error::Conflict)?;
        let reservation = self.entries.reserve().map_err(|error| match error {
            ReserveError::IdentityExhausted => Error::Exhausted,
            _ => Error::Capacity,
        })?;
        let key = reservation.key();
        let id = SpanId(Key::new(nest, key.slot(), key.generation()));
        let span = Span {
            id,
            range,
            phase: SpanPhase::Open,
            root: Pend::held(id),
        };
        assert!(self.entries.publish(reservation, span).is_ok());
        self.tree.insert(key.slot(), position);
        Ok(id)
    }

    fn key(&self, id: SpanId) -> Key<usize> {
        Key::new(self.scope, id.0.slot(), id.0.generation())
    }

    fn original(&mut self, id: SpanId) -> Result<&mut Span, Error> {
        let key = self.key(id);
        self.entries
            .get_mut(key)
            .filter(|span| span.id == id)
            .ok_or(Error::Gone)
    }

    pub(super) fn observe(&mut self, id: SpanId) -> Result<SpanObservation, Error> {
        let span = self.original(id)?;
        Ok(SpanObservation {
            range: span.range,
            phase: span.phase,
        })
    }

    pub(super) fn close(&mut self, id: SpanId) -> Result<(), Error> {
        let span = self.original(id)?;
        if span.phase == SpanPhase::Open {
            assert_eq!(span.root.resolve().expect("original open root"), id);
            span.root.shut();
            span.phase = SpanPhase::Shutting;
        }
        Ok(())
    }

    // Only ordinary empty reservations exist so far. Future Mapping/Memo
    // ownership must settle before this exact root and address are removed.
    pub(super) fn settle(&mut self, id: SpanId) -> Result<(), Error> {
        let span = self.original(id)?;
        if span.phase != SpanPhase::Shutting {
            return Err(Error::State);
        }
        let original = span.root.consume().expect("original root holding");
        assert_eq!(original, id);
        let key = self.key(id);
        self.tree.remove(key.slot());
        self.entries.detach(key).expect("original indexed Span");
        let (reclamation, _) = self.entries.reclaim(key).expect("detached Span");
        self.entries.release(reclamation).expect("settled Span");
        Ok(())
    }

    /// Close or settle one actual address-index member, never a full scan.
    pub(super) fn advance(&mut self) -> bool {
        let Some(slot) = self.tree.first() else {
            return false;
        };
        let (_, span) = self
            .entries
            .published_at(slot.index())
            .expect("indexed Span");
        let id = span.id;
        match span.phase {
            SpanPhase::Open => self.close(id).expect("original open Span"),
            SpanPhase::Shutting => self.settle(id).expect("original closing Span"),
        }
        true
    }
}

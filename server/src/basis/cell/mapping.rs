//! Joint ordinary Mapping admission and exact accepted-use settlement.

use core::ptr;

use limen_treaty::model::{ByteLength, ByteRange, PhysicalAddress};

use crate::{
    basis::nest::MappingId,
    metal::memory::mmu::treaty::{LeafAccess, LocalTranslation, TranslationPhase},
};

use super::{
    AccessId, AccessOwner, AccessPreparation, Admission, CellAccess, CellConstruction, CellSlot,
    Error, Rights, Role, TurnAccess, TurnPlan,
};

fn leaf(rights: Rights) -> Result<LeafAccess, Error> {
    match rights {
        Rights::READ => Ok(LeafAccess::ReadOnly),
        Rights::READ_WRITE => Ok(LeafAccess::ReadWrite),
        _ => Err(Error::Access),
    }
}

fn physical(base: PhysicalAddress, range: ByteRange) -> Result<PhysicalAddress, Error> {
    if range.length() != ByteLength::new(4096) {
        return Err(Error::Access);
    }
    let start = base
        .advance(ByteLength::new(range.start().get()))
        .ok_or(Error::Scope)?;
    if !start.get().is_multiple_of(4096) {
        return Err(Error::Scope);
    }
    Ok(start)
}

/// Discovery snapshot of a real preparation. None of these copied values
/// authorizes acceptance after dropping protection.
pub(in crate::basis) struct MappingInput {
    receiver: &'static AccessPreparation,
    source: &'static CellConstruction,
    slot: &'static CellSlot,
    id: AccessId,
    range: ByteRange,
    rights: Rights,
    physical: PhysicalAddress,
}

impl AccessPreparation {
    pub(in crate::basis) fn mapping(&'static self, id: AccessId) -> Result<MappingInput, Error> {
        let (slot, source) = self.0.acquire(|state| {
            state
                .as_ref()
                .filter(|original| original.access.id == id)
                .map(|original| (original.slot, original.source))
                .ok_or(Error::Scope)
        })??;
        slot.0.acquire(|state| {
            let cell = state.cell.as_ref().ok_or(Error::Scope)?;
            if state.generation != Some(id.0.scope().generation) {
                return Err(Error::Scope);
            }
            let record = state.accesses.ordinary()?.original(id)?;
            if record.owner != self.owner()
                || record.authority != Some(ptr::from_ref(source).addr())
                || record.phase != super::AccessPhase::Prepared
            {
                return Err(Error::Scope);
            }
            let physical = physical(cell.allocation.physical().start(), record.range)?;
            leaf(record.rights)?;
            Ok(MappingInput {
                receiver: self,
                source,
                slot,
                id,
                range: record.range,
                rights: record.rights,
                physical,
            })
        })?
    }
}

impl MappingInput {
    pub(in crate::basis) fn physical(&self) -> PhysicalAddress {
        self.physical
    }

    pub(in crate::basis) fn leaf(&self) -> LeafAccess {
        leaf(self.rights).expect("discovered leaf rights")
    }

    pub(in crate::basis) fn rights(&self) -> Rights {
        self.rights
    }

    pub(in crate::basis) fn plan<'a>(&'a self, plan: &mut TurnPlan<'a, '_>) {
        plan.insert(&self.source.0).expect("original Cell holding");
        self.slot.plan(plan);
        plan.insert(&self.receiver.0)
            .expect("original access receiver");
    }

    /// Final checks precede the single write to AccessRecord and transfer of
    /// the original linear token. The caller has preflighted its actual
    /// Mapping identity, address occupancy and Translation preparation.
    pub(in crate::basis) fn accept(
        &self,
        held: &mut TurnAccess<'_>,
        owner: MappingId,
        translation: &LocalTranslation,
    ) -> Result<AcceptedAccess, Error> {
        let cell_id = self.id.0.scope();
        if translation.phase() != TranslationPhase::Prepared
            || translation.physical() != self.physical
            || translation.access() != self.leaf()
        {
            return Err(Error::Scope);
        }
        let source = held.access(&self.source.0).expect("original Cell holding");
        let original = CellConstruction::original(source, cell_id)?;
        let permit = original.holding.resolve()?;
        if !ptr::eq(original.cell.slot(), self.slot)
            || permit.cell != cell_id
            || permit.role != Role::Ordinary
            || !permit.range.covers(self.range)
            || !permit.rights.covers(self.rights)
        {
            return Err(Error::Scope);
        }
        let (receiver, locked) = self.slot.together(held, &self.receiver.0);
        let original = receiver
            .as_ref()
            .filter(|p| p.access.id == self.id)
            .ok_or(Error::Scope)?;
        if !ptr::eq(original.slot, self.slot) || !ptr::eq(original.source, self.source) {
            return Err(Error::Scope);
        }
        let state = locked.state;
        let cell = state.cell.as_ref().ok_or(Error::Scope)?;
        if state.generation != Some(cell_id.generation)
            || cell.role != Role::Ordinary
            || cell_id.slot != ptr::from_ref(self.slot).addr()
        {
            return Err(Error::Scope);
        }
        if cell.admission != Admission::Open {
            return Err(Error::Shut);
        }
        if physical(cell.allocation.physical().start(), self.range)? != self.physical {
            return Err(Error::Scope);
        }
        let record = state.accesses.ordinary()?.original(self.id)?;
        if record.authority != Some(ptr::from_ref(self.source).addr()) {
            return Err(Error::Scope);
        }
        record.accept(
            &original.access,
            self.range,
            self.rights,
            self.receiver.owner(),
        )?;
        record.owner = AccessOwner::Mapping(owner);
        let original = receiver.take().expect("validated prepared access");
        Ok(AcceptedAccess {
            slot: self.slot,
            access: original.access,
        })
    }
}

/// Linear credential retained by the original Mapping, not by its caller.
#[must_use]
pub(in crate::basis) struct AcceptedAccess {
    slot: &'static CellSlot,
    access: CellAccess,
}

impl AcceptedAccess {
    pub(in crate::basis) fn id(&self) -> AccessId {
        self.access.id
    }

    pub(in crate::basis) fn slot(&self) -> &'static CellSlot {
        self.slot
    }

    /// Complete only this accepted use with the original removed native
    /// Translation. The Mapping owner keeps its own Turn and acknowledges
    /// the use in the same commit.
    pub(in crate::basis) fn finish_locked(
        &self,
        held: &mut super::CellTurn<'_>,
        owner: MappingId,
        translation: &LocalTranslation,
    ) -> Result<(), Error> {
        if translation.phase() != TranslationPhase::Removed {
            return Err(Error::Access);
        }
        if !ptr::eq(held.slot, self.slot) {
            return Err(Error::Scope);
        }
        let state = &mut *held.state;
        let id = self.access.id;
        let cell = state.cell.as_ref().ok_or(Error::Scope)?;
        if state.generation != Some(id.0.scope().generation) || cell.role != Role::Ordinary {
            return Err(Error::Scope);
        }
        let base = cell.allocation.physical().start();
        let store = state.accesses.ordinary()?;
        let record = store.original(id)?;
        record.accepted(&self.access)?;
        if record.owner != AccessOwner::Mapping(owner)
            || physical(base, record.range)? != translation.physical()
            || leaf(record.rights)? != translation.access()
        {
            return Err(Error::Scope);
        }
        let key = super::Key::new(store.scope, id.0.slot(), id.0.generation());
        store.remove(key);
        Ok(())
    }
}

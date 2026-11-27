//! Original stream and VMID reservations and their directory effects.

use super::{
    Command, Controller, DeviceOrder, Effect, Error, Refresh, State,
    directory::{Directory, StreamTable},
    translation::Format,
};
use limen_treaty::model::PhysicalAddress;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub stream: u32,
    pub root: PhysicalAddress,
    pub vmid: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub(super) controller: PhysicalAddress,
    pub(super) generation: u64,
    pub(super) context: Context,
}

impl Binding {
    pub fn context(self) -> Context {
        self.context
    }
}

pub(super) struct Bindings<'a> {
    directory: Directory,
    records: &'a mut [Option<Binding>],
}

impl<'a> Bindings<'a> {
    pub(super) fn new(
        directory: Directory,
        records: &'a mut [Option<Binding>],
    ) -> Result<Self, Error> {
        // One record for each admitted stream and its distinct 8-bit VMID.
        if records.is_empty() || records.len() > directory.len().min(256) {
            return Err(Error::Capacity);
        }
        if records.iter().any(Option::is_some) || !directory.empty() {
            return Err(Error::Active);
        }
        Ok(Self { directory, records })
    }

    pub(super) fn table(&self) -> StreamTable {
        self.directory.table()
    }

    pub(super) fn occupied(&self) -> bool {
        self.records.iter().any(Option::is_some)
    }

    fn lookup(&self, stream: u32) -> Option<Binding> {
        self.records
            .iter()
            .flatten()
            .find(|binding| binding.context.stream == stream)
            .copied()
    }

    fn prepare(&self, format: Format, context: Context) -> Result<(usize, [u64; 8]), Error> {
        let entry = self.directory.entry(context.stream)?;
        if self.records.iter().flatten().any(|binding| {
            binding.context.stream == context.stream || binding.context.vmid == context.vmid
        }) {
            return Err(Error::Active);
        }
        if !entry.empty() {
            return Err(Error::Unconfirmed);
        }
        let slot = self
            .records
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        Ok((slot, format.stream(context.root, context.vmid)?))
    }

    fn publish(&mut self, slot: usize, binding: Binding, words: [u64; 8], order: &DeviceOrder) {
        self.records[slot] = Some(binding);
        self.directory.publish(binding.context.stream, words, order);
    }

    fn block(&mut self, binding: Binding, order: &DeviceOrder) {
        // The page walk is blocked now, but this stream and VMID still
        // belong to the original binding until its maintenance settles.
        self.directory.block(binding.context.stream, order);
    }

    fn retire(&mut self, binding: Binding, order: &DeviceOrder) {
        let record = self
            .records
            .iter_mut()
            .find(|record| **record == Some(binding))
            .expect("retained original binding");
        self.directory.retire(binding.context.stream, order);
        *record = None;
    }
}

impl Controller<'_> {
    fn binding_for(&self, context: Context, generation: u64) -> Binding {
        Binding {
            controller: self.registers.physical().start(),
            generation,
            context,
        }
    }

    pub fn binding(&self, stream: u32) -> Option<Binding> {
        if !matches!(self.state, State::Enabled)
            || self
                .commands
                .effect()
                .is_some_and(|effect| effect.hides_binding(stream))
        {
            return None;
        }
        self.bindings.lookup(stream)
    }

    /// Fill one STE before installing the table.
    ///
    /// # Safety
    /// Retain the authorized stage-two translation and checked routing for the
    /// original stream. Its subordinate pages must remain pinned and visible
    /// until the binding and independent endpoint obligations have completed.
    pub unsafe fn prepare(&mut self, context: Context) -> Result<Binding, Error> {
        if !matches!(self.state, State::Blocked) {
            return Err(Error::Active);
        }
        let (slot, words) = self.bindings.prepare(self.format()?, context)?;
        let generation = self.sequence.prepare_binding()?;
        let binding = self.binding_for(context, generation);
        self.bindings.publish(slot, binding, words, &self.order);
        Ok(binding)
    }

    fn available_queue(&self) -> Result<u64, Error> {
        if !matches!(self.state, State::Enabled) {
            return Err(Error::Active);
        }
        if self.commands.command().is_some() {
            return Err(Error::Busy);
        }
        self.queue.as_ref().ok_or(Error::Active)?.open_generation()
    }

    fn reserve(&mut self, queue: u64, effect: Effect) -> Result<Command, Error> {
        self.commands.reserve(
            self.registers.physical().start(),
            queue,
            effect,
            self.sequence,
        )
    }

    /// Publish a new STE and reserve its exact configuration and TLB commands.
    ///
    /// # Safety
    /// The stream remains quiescent until this command settles; retain the
    /// translation and exact routing as required by prepare.
    pub unsafe fn begin_bind(&mut self, context: Context) -> Result<Command, Error> {
        let queue = self.available_queue()?;
        let (slot, words) = self.bindings.prepare(self.format()?, context)?;
        let generation = self.sequence.next()?;
        let binding = self.binding_for(context, generation);
        let command = self.reserve(queue, Effect::Bound(binding))?;
        self.bindings.publish(slot, binding, words, &self.order);
        Ok(command)
    }

    /// Associate one new maintenance command with the exact current binding.
    ///
    /// # Safety
    /// Subordinate page-table changes are visible and both old and new pages
    /// remain pinned until the original command and endpoint effects complete.
    pub unsafe fn begin_refresh(
        &mut self,
        binding: Binding,
        refresh: Refresh,
    ) -> Result<Command, Error> {
        if self.bindings.lookup(binding.context.stream) != Some(binding) {
            return Err(Error::Stale);
        }
        let queue = self.available_queue()?;
        self.reserve(queue, Effect::Refreshed(binding, refresh))
    }

    /// Block and invalidate an exact original binding.
    ///
    /// # Safety
    /// Stop and drain the original stream, with no ATS or endpoint cache
    /// that can still refer to its binding. Retain all original storage
    /// until its original cache invalidations and independent users settle.
    pub unsafe fn begin_unbind(&mut self, binding: Binding) -> Result<Command, Error> {
        if self.bindings.lookup(binding.context.stream) != Some(binding) {
            return Err(Error::Stale);
        }
        let queue = self.available_queue()?;
        let command = self.reserve(queue, Effect::Unbound(binding))?;
        self.bindings.block(binding, &self.order);
        Ok(command)
    }

    pub fn observe(&mut self, command: Command) -> Result<Effect, Error> {
        let effect = self.commands.observe(
            command,
            self.queue.as_mut(),
            &mut self.registers,
            &self.order,
        )?;
        if let Effect::Unbound(binding) = effect {
            self.bindings.retire(binding, &self.order);
        }
        Ok(effect)
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/aarch64/external/smmu/binding.rs"]
mod tests;

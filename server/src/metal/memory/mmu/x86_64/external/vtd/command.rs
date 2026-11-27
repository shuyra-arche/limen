//! Legacy register-command issuance, observation, and root installation.

use super::{
    CONTEXT, Controller, Error, GLOBAL_CONTEXT, GLOBAL_IOTLB, IN_PROGRESS, ROOT, ROOT_POINTER,
    State, TRANSLATION, binding::Binding,
};
use limen_treaty::model::PhysicalAddress;

/// The exact binding affected by an issued command sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Change {
    Bound(Binding),
    Invalidated(Binding),
    Unbound(Binding),
}

/// The original register-command sequence on this physical controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandId {
    pub(super) unit: PhysicalAddress,
    pub(super) number: u64,
}

pub(super) enum Operation {
    Bind(usize),
    Invalidate(usize),
    Unbind(usize),
}

impl Controller<'_> {
    pub(super) fn invalidate_contexts(&mut self) {
        self.state = State::RefreshingContexts;
        self.write(CONTEXT, IN_PROGRESS | GLOBAL_CONTEXT);
    }

    pub fn observe(&mut self, expected: CommandId) -> Result<Change, Error> {
        let (command, completion) = self.pending().ok_or(Error::Stale)?;
        if command != expected {
            return Err(Error::Stale);
        }
        match self.state {
            State::RefreshingContexts => {
                self.context_complete()?;
                self.state = State::RefreshingTranslations;
                self.write(self.iotlb, IN_PROGRESS | GLOBAL_IOTLB | self.drain);
            }
            State::RefreshingTranslations => {}
            State::Unconfirmed => return Err(Error::Unconfirmed),
            _ => return Err(Error::Occupied),
        }
        self.translations_complete()?;
        if let Some((_, Operation::Unbind(slot))) = self.pending.take() {
            self.storage.bindings[slot] = None;
        }
        self.state = State::Enabled;
        Ok(completion)
    }

    pub fn unconfirmed_phase(&self) -> Option<State> {
        self.unconfirmed
    }

    fn context_complete(&mut self) -> Result<(), Error> {
        let actual = self.read(CONTEXT);
        if actual & IN_PROGRESS != 0 {
            return Err(Error::Pending);
        }
        if actual >> 59 & 3 != 1 {
            return Err(self.unconfirmed());
        }
        Ok(())
    }

    fn translations_complete(&mut self) -> Result<(), Error> {
        let actual = self.read(self.iotlb);
        if actual & IN_PROGRESS != 0 {
            return Err(Error::Pending);
        }
        if actual >> 57 & 3 != 1 {
            return Err(self.unconfirmed());
        }
        Ok(())
    }

    /// Install the retained, initially denying legacy root. Pending retries
    /// continue the original publication and preserve its pages.
    ///
    /// # Safety
    /// The entire aligned 4 KiB root must be addressable by this controller,
    /// initialized with valid scoped entries, visible to its walker and retained while
    /// installed or uncertain, including after this handle is dropped. All
    /// descendant tables, domain IDs and mapped pages must stay live for
    /// their hardware obligations. All covered requesters and the upstream
    /// interconnect must be quiesced through the transition.
    /// The operation does not cover ATS caches or revoke prior device access.
    pub unsafe fn install_legacy(&mut self) -> Result<(), Error> {
        let physical = self.storage.root.1;
        if self.state == State::Idle {
            let entry = &mut self.storage.root.0.0[usize::from(self.storage.bus)];
            let target = self
                .format
                .root_entry(self.storage.contexts.1)
                .ok_or(Error::Root)?;
            self.root = Some(physical);
            self.state = State::LoadingRoot;
            *entry = target;
            self.visibility.publish(&*self.storage.contexts.0);
            self.visibility.publish(&*self.storage.root.0);
            self.write(ROOT, physical.get());
            self.command(ROOT_POINTER);
        }
        for _ in 0..256 {
            let state = self.state;
            match state {
                State::LoadingRoot if self.status() & ROOT_POINTER != 0 => {
                    if self.read(ROOT) != physical.get() {
                        return Err(self.unconfirmed());
                    }
                    self.state = State::InvalidatingContext;
                    self.write(CONTEXT, IN_PROGRESS | GLOBAL_CONTEXT);
                }
                State::InvalidatingContext => match self.context_complete() {
                    Ok(()) => {
                        self.state = State::InvalidatingTranslations;
                        self.write(self.iotlb, IN_PROGRESS | GLOBAL_IOTLB | self.drain);
                    }
                    Err(Error::Pending) => core::hint::spin_loop(),
                    Err(error) => return Err(error),
                },
                State::InvalidatingTranslations => match self.translations_complete() {
                    Ok(()) => {
                        self.state = State::Enabling;
                        self.command(TRANSLATION);
                    }
                    Err(Error::Pending) => core::hint::spin_loop(),
                    Err(error) => return Err(error),
                },
                State::Enabling if self.status() & TRANSLATION != 0 => {
                    self.state = State::Enabled;
                    return Ok(());
                }
                State::Enabled => return Ok(()),
                State::RefreshingContexts | State::RefreshingTranslations => {
                    return Err(Error::Occupied);
                }
                State::Unconfirmed => return Err(Error::Unconfirmed),
                _ => core::hint::spin_loop(),
            }
        }
        Err(Error::Pending)
    }
}

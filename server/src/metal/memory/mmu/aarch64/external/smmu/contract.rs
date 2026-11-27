//! Shared observation of the controller's original binding commands.

use super::{Binding, Command, Context, Controller, Effect, Error, Refresh};
use crate::metal::memory::mmu::treaty::{
    BindingChange, BindingCommand, BindingControl, BindingEffect, BindingObservation,
    BindingRejection, BindingSubmission, binding::Sealed,
};

impl From<Effect> for BindingEffect<Binding, Refresh> {
    fn from(effect: Effect) -> Self {
        match effect {
            Effect::Bound(binding) => Self::Bound(binding),
            Effect::Refreshed(binding, scope) => Self::Refreshed(binding, scope),
            Effect::Unbound(binding) => Self::Unbound(binding),
        }
    }
}

impl Sealed for Controller<'_> {}

impl BindingControl for Controller<'_> {
    type Context = Context;
    type Binding = Binding;
    type Refresh = Refresh;
    type Command = Command;
    type Error = Error;

    unsafe fn begin_binding(&mut self, context: Context) -> BindingSubmission<Self> {
        let command = unsafe { self.begin_bind(context) }.map_err(BindingRejection::Rejected)?;
        let effect = self.unsettled_effect().expect("accepted binding");
        Ok(BindingCommand::new(command, effect.into()))
    }

    unsafe fn begin_refresh(
        &mut self,
        binding: Binding,
        scope: Refresh,
    ) -> BindingSubmission<Self> {
        unsafe { Controller::begin_refresh(self, binding, scope) }
            .map(|command| BindingCommand::new(command, BindingEffect::Refreshed(binding, scope)))
            .map_err(BindingRejection::Rejected)
    }

    unsafe fn begin_retirement(&mut self, binding: Binding) -> BindingSubmission<Self> {
        unsafe { self.begin_unbind(binding) }
            .map(|command| BindingCommand::new(command, BindingEffect::Unbound(binding)))
            .map_err(BindingRejection::Rejected)
    }

    fn unsettled_change(&self) -> Option<BindingChange<Self>> {
        Some(BindingCommand::new(
            self.pending_command()?,
            self.unsettled_effect()?.into(),
        ))
    }

    fn observe_change(&mut self, change: BindingChange<Self>) -> BindingObservation<Self> {
        change.observe(self.unsettled_change(), |command| {
            match self.observe(command) {
                Ok(effect) => Ok(Some(effect.into())),
                Err(Error::Busy) => Ok(None),
                Err(error) => Err(error),
            }
        })
    }
}

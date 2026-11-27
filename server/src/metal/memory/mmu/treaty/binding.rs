//! The lifecycle of an exact requester binding on its native controller.

pub trait Sealed {}

pub trait BindingControl: Sealed + Sized {
    type Context;
    type Binding: Copy + Eq;
    /// The controller's supported refresh scopes. A controller without
    /// refresh uses an uninhabited scope.
    type Refresh: Copy + Eq;
    type Command: Copy + Eq;
    type Error;

    /// Establish a binding and return the accepted publication.
    ///
    /// # Safety
    /// Quiesce the requester through publication. Retain the routing,
    /// translation storage and domains under the native controller's rules.
    unsafe fn begin_binding(&mut self, context: Self::Context) -> BindingSubmission<Self>;

    /// Publish maintenance for this exact binding.
    ///
    /// # Safety
    /// Make the table edits visible and retain every potentially observed
    /// translation through confirmation. Meet the native scope's requester,
    /// endpoint-cache and fabric ordering requirements.
    unsafe fn begin_refresh(
        &mut self,
        binding: Self::Binding,
        scope: Self::Refresh,
    ) -> BindingSubmission<Self>;

    /// Block this generation of the requester and start retiring it.
    ///
    /// # Safety
    /// Stop and drain affected traffic and keep the original backing until
    /// this retirement confirms and independent endpoint obligations settle.
    unsafe fn begin_retirement(&mut self, binding: Self::Binding) -> BindingSubmission<Self>;

    /// Recover the original command and effect still held by this controller,
    /// including an unconfirmed effect. This does not publish new work.
    fn unsettled_change(&self) -> Option<BindingChange<Self>>;

    /// Recover work owned by the exact binding, including its generation.
    fn pending_change(&self, binding: Self::Binding) -> Option<BindingChange<Self>> {
        self.unsettled_change()
            .filter(|change| change.effect().binding() == binding)
    }

    /// Advance and observe only this original command.
    fn observe_change(&mut self, change: BindingChange<Self>) -> BindingObservation<Self>;
}

pub type BindingChange<C> = BindingCommand<
    <C as BindingControl>::Binding,
    <C as BindingControl>::Refresh,
    <C as BindingControl>::Command,
>;

pub type BindingSubmission<C> =
    Result<BindingChange<C>, BindingRejection<BindingChange<C>, <C as BindingControl>::Error>>;

pub type BindingObservation<C> = BindingProgress<
    <C as BindingControl>::Binding,
    <C as BindingControl>::Refresh,
    <C as BindingControl>::Error,
>;

/// A controller-issued observation handle. Its binding and command carry
/// the original hardware scope and the controller's exact generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindingCommand<B, R, I> {
    command: I,
    effect: BindingEffect<B, R>,
}

impl<B, R, I> BindingCommand<B, R, I> {
    pub(in crate::metal::memory::mmu) const fn new(
        command: I,
        effect: BindingEffect<B, R>,
    ) -> Self {
        Self { command, effect }
    }

    pub fn command(self) -> I {
        self.command
    }

    pub fn effect(self) -> BindingEffect<B, R> {
        self.effect
    }
}

impl<B: Eq, R: Eq, I: Eq> BindingCommand<B, R, I> {
    pub(in crate::metal::memory::mmu) fn observe<E>(
        self,
        unsettled: Option<Self>,
        advance: impl FnOnce(I) -> Result<Option<BindingEffect<B, R>>, E>,
    ) -> BindingProgress<B, R, E> {
        if unsettled.as_ref() != Some(&self) {
            return BindingProgress::Stale;
        }
        match advance(self.command) {
            Ok(Some(effect)) => {
                assert!(
                    effect == self.effect,
                    "different effect for the original binding command"
                );
                BindingProgress::Confirmed(effect)
            }
            Ok(None) => BindingProgress::Pending,
            Err(error) => BindingProgress::Unconfirmed(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingEffect<B, R> {
    Bound(B),
    Refreshed(B, R),
    Unbound(B),
}

impl<B, R> BindingEffect<B, R> {
    pub fn binding(self) -> B {
        match self {
            Self::Bound(binding) | Self::Refreshed(binding, _) | Self::Unbound(binding) => binding,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingRejection<T, E> {
    /// The initiation left ownership and the hardware effect unchanged.
    Rejected(E),
    /// Initiation retained this exact effect, but its hardware outcome
    /// is uncertain. Keep the affected backing and admission reserved.
    Unconfirmed { change: T, error: E },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingProgress<B, R, E> {
    Pending,
    Confirmed(BindingEffect<B, R>),
    /// This handle names no unsettled command on this controller.
    Stale,
    /// The original effect and backing remain held by the controller.
    Unconfirmed(E),
}

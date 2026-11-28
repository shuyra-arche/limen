//! An original fixed-vector sender and the destination's ordinary idle window.

use core::{marker::PhantomData, ptr};

use crate::metal::processor::treaty::{ProcessorId, RuntimeTrap, can_switch, wait_for_interrupt};

use super::reception::{SOFTWARE_ENABLE, SPURIOUS, SUPPRESS_BROADCAST};
use super::{Failure, IpiRoute, IpiTarget, LocalApic, Reception, VectorLease};

const PRIORITY: usize = 0xa0;

/// One permanent receive reservation and its one assigned physical sender.
/// The controller retains the vector for the admission's lifetime.
#[must_use]
pub struct WakeReceiver {
    target: IpiTarget,
    source: ProcessorId,
    runtime: &'static RuntimeTrap,
    lease: VectorLease,
}

impl WakeReceiver {
    /// # Safety
    /// The selected vector belongs exclusively to the specified original
    /// sender and this live Hook receiver. Keep both original APIC owners,
    /// the installed runtime and the route resident through reception. All
    /// selected-vector edges come from issues by that sender. The receiver
    /// records actual hardware deliveries and settles their original EOI
    /// before returning. Exclude other ordinary delivery and configuration
    /// changes while opening IF for this receiver.
    pub unsafe fn admit(
        controller: &mut LocalApic<'_>,
        target: IpiTarget,
        source: ProcessorId,
    ) -> Result<Self, Failure> {
        let destination = ProcessorId::new(usize::from(target.processor()));
        let runtime =
            RuntimeTrap::idle_entry(destination, target.vector()).ok_or(Failure::Scope)?;
        if controller.processor != destination.get()
            || source == destination
            || target.controller() != controller.base.physical.start()
        {
            return Err(Failure::Scope);
        }
        controller.check_wake_gate(target.vector())?;
        if controller.reception_round != 0 || controller.vectors != [0; 4] {
            return Err(Failure::Busy);
        }
        let lease = controller.reserve_vector(target.vector())?;
        Ok(Self {
            target,
            source,
            runtime,
            lease,
        })
    }

    /// Bind the actual assigned sender to the installed, resident receiver.
    /// A firmware destination description alone cannot activate this route.
    pub fn bind_sender<'a, 'map>(
        &'static self,
        controller: &'a mut LocalApic<'map>,
    ) -> Result<IpiRoute<'a, 'map>, Failure> {
        if controller.processor != self.source.get() {
            return Err(Failure::Scope);
        }
        unsafe { controller.bind_ipi(self.target) }
    }

    pub fn vector(&self) -> u8 {
        self.lease.vector()
    }

    /// Verify the actual receiver before ending its ordinary register borrow.
    pub fn idle<'receiver>(
        &'receiver self,
        controller: &mut LocalApic<'_>,
    ) -> Result<Idle<'receiver>, Failure> {
        self.check_local()?;
        if controller.processor != usize::from(self.target.processor())
            || controller.base.physical.start() != self.target.controller()
        {
            return Err(Failure::Scope);
        }
        controller.check_wake_gate(self.vector())?;
        Ok(Idle {
            receiver: self,
            local: PhantomData,
        })
    }

    fn check_local(&self) -> Result<(), Failure> {
        let destination = ProcessorId::new(usize::from(self.target.processor()));
        let runtime = RuntimeTrap::idle_entry(destination, self.vector()).ok_or(Failure::Scope)?;
        if !ptr::eq(runtime, self.runtime) {
            return Err(Failure::Scope);
        }
        if !can_switch() {
            return Err(Failure::Busy);
        }
        Ok(())
    }
}

impl LocalApic<'_> {
    fn check_wake_gate(&mut self, vector: u8) -> Result<(), Failure> {
        self.reception_context()?;
        let spurious = unsafe { self.window.read_u32(SPURIOUS) }.expect("xAPIC SVR");
        let priority = unsafe { self.window.read_u32(PRIORITY) }.expect("xAPIC PPR");
        if vector == spurious as u8
            || spurious & (SOFTWARE_ENABLE | SUPPRESS_BROADCAST) != SOFTWARE_ENABLE
        {
            return Err(Failure::Scope);
        }
        if self.reception != Reception::Idle
            || priority & 0xf0 >= u32::from(vector & 0xf0)
            || self.top().is_some()
        {
            return Err(Failure::Busy);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wake {
    Ready,
    Recheck,
}

/// The current processor's verified receive window after its APIC borrow ends.
#[must_use]
pub struct Idle<'receiver> {
    receiver: &'receiver WakeReceiver,
    local: PhantomData<*mut ()>,
}

impl Idle<'_> {
    /// Recheck the original publication on both sides of a single native wait.
    ///
    /// # Safety
    /// Keep the verified route and its priority fixed through this window.
    /// The bounded check must leave no local protection or reachable receiver
    /// borrow. Work published after the last negative check must retain a live
    /// notification path. The caller has no active Unit that this wait stops.
    pub unsafe fn wait(self, mut ready: impl FnMut() -> bool) -> Result<Wake, Failure> {
        self.receiver.check_local()?;
        if ready() {
            return Ok(Wake::Ready);
        }
        self.receiver.check_local()?;
        unsafe { wait_for_interrupt() };
        self.receiver.check_local()?;
        Ok(if ready() { Wake::Ready } else { Wake::Recheck })
    }
}

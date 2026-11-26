//! Ordinary execution protection owned by the initialized physical CPU.

use core::{cell::Cell, marker::PhantomData, mem::DropGuard, sync::atomic::AtomicBool};

use super::{HartLocal, QueueNode, backend, installed, ordinary_entry};

/// The fixed queue-node storage supplied by the current boot image per Hart.
/// Owners check their maximum joint acquisition against this supply.
pub(crate) const TURN_CAPACITY: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    Capacity,
    Context,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Acquiring,
    Held,
    Releasing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Participation {
    Idle,
    Turn {
        count: usize,
        phase: Phase,
    },
    Leaf {
        location: *const AtomicBool,
        phase: Phase,
        turn: Option<usize>,
    },
}

pub(super) struct LocalProtection {
    current: Cell<Participation>,
    interrupt: Cell<Option<usize>>,
    nodes: [QueueNode; TURN_CAPACITY],
}

impl LocalProtection {
    pub(super) const fn new() -> Self {
        Self {
            current: Cell::new(Participation::Idle),
            interrupt: Cell::new(None),
            nodes: [const { QueueNode::new() }; TURN_CAPACITY],
        }
    }

    fn save(&self, interrupt: usize) {
        assert!(self.interrupt.replace(Some(interrupt)).is_none());
    }

    fn settle(&self) {
        // Clear all ordinary state before restored IRQ can dispatch again.
        self.current.set(Participation::Idle);
        let interrupt = self.interrupt.take().expect("outer local protection");
        unsafe { backend::restore_interrupts(interrupt) };
    }
}

type InterruptGuard = DropGuard<usize, fn(usize)>;

fn protect() -> Result<(&'static HartLocal, InterruptGuard), Error> {
    if !ordinary_entry() {
        return Err(Error::Context);
    }
    let interrupt: InterruptGuard =
        DropGuard::new(unsafe { backend::mask_interrupts() }, |saved| unsafe {
            backend::restore_interrupts(saved)
        });
    Ok((installed().ok_or(Error::Context)?, interrupt))
}

pub(crate) fn can_switch() -> bool {
    let Ok((local, _interrupt)) = protect() else {
        return false;
    };
    local.protection.current.get() == Participation::Idle
        && local.protection.interrupt.get().is_none()
}

pub(crate) struct TurnParticipation {
    local: &'static LocalProtection,
    count: usize,
    same_hart: PhantomData<*mut ()>,
}

pub(crate) fn begin_turn(count: usize) -> Result<TurnParticipation, Error> {
    if count == 0 || count > TURN_CAPACITY {
        return Err(Error::Capacity);
    }
    let (local, interrupt) = protect()?;
    let local = &local.protection;
    if local.current.get() != Participation::Idle {
        return Err(Error::Context);
    }
    local.save(DropGuard::dismiss(interrupt));
    local.current.set(Participation::Turn {
        count,
        phase: Phase::Acquiring,
    });
    Ok(TurnParticipation {
        local,
        count,
        same_hart: PhantomData,
    })
}

impl TurnParticipation {
    pub(crate) fn node(&self, index: usize) -> &QueueNode {
        &self.local.nodes[..self.count][index]
    }

    pub(crate) fn held(&self) {
        assert_eq!(self.local.current.get(), self.state(Phase::Acquiring));
        self.local.current.set(self.state(Phase::Held));
    }

    pub(crate) fn releasing(&self) {
        let current = self.local.current.get();
        assert!(current == self.state(Phase::Acquiring) || current == self.state(Phase::Held));
        self.local.current.set(self.state(Phase::Releasing));
    }

    fn state(&self, phase: Phase) -> Participation {
        Participation::Turn {
            count: self.count,
            phase,
        }
    }
}

impl Drop for TurnParticipation {
    fn drop(&mut self) {
        // Turn retains private responsibility for detaching the acquired
        // prefix before relinquishing this admission, including on Busy.
        assert_eq!(self.local.current.get(), self.state(Phase::Releasing));
        self.local.settle();
    }
}

pub(crate) struct LeafParticipation {
    local: &'static LocalProtection,
    location: *const AtomicBool,
    turn: Option<usize>,
    same_hart: PhantomData<*mut ()>,
}

pub(crate) fn begin_leaf(
    location: *const AtomicBool,
    wait: bool,
) -> Result<LeafParticipation, Error> {
    let (local, interrupt) = protect()?;
    let local = &local.protection;
    let turn = match local.current.get() {
        Participation::Idle => {
            local.save(DropGuard::dismiss(interrupt));
            None
        }
        Participation::Turn {
            count,
            phase: Phase::Held,
        } if !wait => {
            // The enclosing Turn already owns the original interrupt state.
            DropGuard::dismiss(interrupt);
            Some(count)
        }
        _ => return Err(Error::Context),
    };
    let participation = LeafParticipation {
        local,
        location,
        turn,
        same_hart: PhantomData,
    };
    local.current.set(participation.state(Phase::Acquiring));
    Ok(participation)
}

impl LeafParticipation {
    pub(crate) fn held(&self) {
        assert_eq!(self.local.current.get(), self.state(Phase::Acquiring));
        self.local.current.set(self.state(Phase::Held));
    }

    pub(crate) fn releasing(&self) {
        assert_eq!(self.local.current.get(), self.state(Phase::Held));
        self.local.current.set(self.state(Phase::Releasing));
    }

    fn state(&self, phase: Phase) -> Participation {
        Participation::Leaf {
            location: self.location,
            phase,
            turn: self.turn,
        }
    }
}

impl Drop for LeafParticipation {
    fn drop(&mut self) {
        let current = self.local.current.get();
        assert!(current == self.state(Phase::Acquiring) || current == self.state(Phase::Releasing));
        if let Some(count) = self.turn {
            self.local.current.set(Participation::Turn {
                count,
                phase: Phase::Held,
            });
        } else {
            self.local.settle();
        }
    }
}

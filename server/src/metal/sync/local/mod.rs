//! Local participation in the ordinary synchronization protocols.

use core::{
    cell::{Cell, OnceCell},
    sync::atomic::AtomicBool,
};

use crate::metal::processor::treaty::{Protection, protect};

use super::turn::node::QueueNode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
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

pub(in crate::metal) struct LocalSync {
    current: Cell<Participation>,
    nodes: OnceCell<&'static [QueueNode]>,
}

/// Fixed synchronization material supplied to one entering CPU.
#[cfg_attr(
    not(any(limen_target, test)),
    expect(
        dead_code,
        reason = "resident synchronization supply is used by CPU startup"
    )
)]
pub struct Supply<const N: usize> {
    local: LocalSync,
    nodes: [QueueNode; N],
}

impl<const N: usize> Supply<N> {
    pub const fn new() -> Self {
        Self {
            local: LocalSync {
                current: Cell::new(Participation::Idle),
                nodes: OnceCell::new(),
            },
            nodes: [const { QueueNode::new() }; N],
        }
    }

    #[cfg(any(limen_target, test))]
    pub(in crate::metal) const fn material(&'static self) -> Material {
        Material {
            local: &self.local,
            nodes: &self.nodes,
        }
    }
}

impl<const N: usize> Default for Supply<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// The complete original supply, with its fixed node bound carried by the slice.
#[cfg(any(limen_target, test))]
pub(in crate::metal) struct Material {
    local: &'static LocalSync,
    nodes: &'static [QueueNode],
}

// The descriptor allows mutation only through exclusive CPU startup. Nodes
// subsequently synchronize only through their own atomic queue links.
#[cfg(any(limen_target, test))]
unsafe impl Sync for Material {}

#[cfg(any(limen_target, test))]
impl Material {
    /// # Safety
    /// Bind once under exclusive startup, before the owning CPU can expose
    /// the record to ordinary operations. The supply remains fixed thereafter.
    pub(in crate::metal) unsafe fn bind(&self) -> &'static LocalSync {
        assert!(self.local.nodes.set(self.nodes).is_ok());
        self.local
    }
}

impl LocalSync {
    pub(super) fn capacity(&self) -> usize {
        self.nodes().len()
    }

    fn nodes(&self) -> &'static [QueueNode] {
        self.nodes
            .get()
            .copied()
            .expect("installed synchronization supply")
    }
}

pub(super) struct TurnParticipation {
    local: &'static LocalSync,
    nodes: &'static [QueueNode],
    count: usize,
    _protection: Protection,
}

pub(super) fn begin_turn(count: usize) -> Result<TurnParticipation, Error> {
    if count == 0 {
        return Err(Error::Capacity);
    }
    let protection = protect().ok_or(Error::Context)?;
    let local = protection.synchronization().ok_or(Error::Context)?;
    let nodes = local.nodes();
    if count > nodes.len() {
        return Err(Error::Capacity);
    }
    if local.current.get() != Participation::Idle {
        return Err(Error::Context);
    }
    local.current.set(Participation::Turn {
        count,
        phase: Phase::Acquiring,
    });
    Ok(TurnParticipation {
        local,
        nodes,
        count,
        _protection: protection,
    })
}

impl TurnParticipation {
    pub(super) fn node(&self, index: usize) -> &QueueNode {
        &self.nodes[..self.count][index]
    }

    pub(super) fn held(&self) {
        assert_eq!(self.local.current.get(), self.state(Phase::Acquiring));
        self.local.current.set(self.state(Phase::Held));
    }

    pub(super) fn releasing(&self) {
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
        self.local.current.set(Participation::Idle);
        // The protection is released after this participant's accounting.
    }
}

pub(super) struct LeafParticipation {
    local: &'static LocalSync,
    location: *const AtomicBool,
    turn: Option<usize>,
    _protection: Protection,
}

pub(super) fn begin_leaf(
    location: *const AtomicBool,
    wait: bool,
) -> Result<LeafParticipation, Error> {
    let protection = protect().ok_or(Error::Context)?;
    let local = protection.synchronization().ok_or(Error::Context)?;
    let turn = match local.current.get() {
        Participation::Idle => None,
        Participation::Turn {
            count,
            phase: Phase::Held,
        } if !wait => Some(count),
        _ => return Err(Error::Context),
    };
    let participation = LeafParticipation {
        local,
        location,
        turn,
        _protection: protection,
    };
    local.current.set(participation.state(Phase::Acquiring));
    Ok(participation)
}

impl LeafParticipation {
    pub(super) fn held(&self) {
        assert_eq!(self.local.current.get(), self.state(Phase::Acquiring));
        self.local.current.set(self.state(Phase::Held));
    }

    pub(super) fn releasing(&self) {
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
            self.local.current.set(Participation::Idle);
        }
    }
}

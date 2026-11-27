//! Stable address indexing for this source's append-only Region directory.

use limen_treaty::model::{ByteLength, PhysicalRange};

use crate::metal::identity::Slot;

use super::{Error, Region};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Color {
    Red,
    Black,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Side {
    Left,
    Right,
}

impl Side {
    fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// Prepaid links at the stable Region position. No copied range or identity.
#[derive(Clone, Copy)]
pub(super) struct Link {
    parent: Option<Slot>,
    left: Option<Slot>,
    right: Option<Slot>,
    color: Color,
}

impl Link {
    pub(super) const EMPTY: Self = Self {
        parent: None,
        left: None,
        right: None,
        color: Color::Black,
    };
}

/// The exact insertion decision remains private to the held directory.
pub(super) struct Insertion {
    parent: Option<(Slot, Side)>,
    managed: ByteLength,
}

pub(super) struct Directory {
    root: Option<Slot>,
    links: &'static mut [Link],
    managed: ByteLength,
}

impl Directory {
    pub(super) fn new(links: &'static mut [Link]) -> Self {
        Self {
            root: None,
            links,
            managed: ByteLength::new(0),
        }
    }

    /// Existing links resolve only to stable initialized descriptions.
    /// The caller keeps the same directory protection through insertion.
    pub(super) fn prepare(
        &self,
        regions: &[Region],
        range: PhysicalRange,
        maximum: ByteLength,
    ) -> Result<Insertion, Error> {
        let mut parent = None;
        let mut current = self.root;
        while let Some(slot) = current {
            let physical = regions[slot.index()].description().physical;
            let side = if range.end() <= physical.start() {
                Side::Left
            } else if range.start() >= physical.end() {
                Side::Right
            } else {
                return Err(Error::Overlap);
            };
            parent = Some((slot, side));
            current = self.child(slot, side);
        }
        let managed = self
            .managed
            .checked_add(range.length())
            .ok_or(Error::Capacity)?;
        if managed > maximum {
            return Err(Error::Capacity);
        }
        Ok(Insertion { parent, managed })
    }

    /// Install the exact next unpublished Region position after preflight.
    /// The new Region is initialized; the same directory remains protected.
    pub(super) fn insert(&mut self, slot: Slot, plan: Insertion) {
        let parent = plan.parent.map(|(parent, _)| parent);
        self.links[slot.index()] = Link {
            parent,
            color: Color::Red,
            ..Link::EMPTY
        };
        match plan.parent {
            Some((parent, side)) => {
                assert!(self.child(parent, side).is_none());
                self.set_child(parent, side, Some(slot));
            }
            None => {
                assert!(self.root.is_none());
                self.root = Some(slot);
            }
        }
        self.rebalance(slot);
        self.managed = plan.managed;
    }

    fn child(&self, slot: Slot, side: Side) -> Option<Slot> {
        let link = &self.links[slot.index()];
        match side {
            Side::Left => link.left,
            Side::Right => link.right,
        }
    }

    fn set_child(&mut self, slot: Slot, side: Side, child: Option<Slot>) {
        let link = &mut self.links[slot.index()];
        match side {
            Side::Left => link.left = child,
            Side::Right => link.right = child,
        }
    }

    fn parent(&self, slot: Slot) -> Option<Slot> {
        self.links[slot.index()].parent
    }

    fn side(&self, parent: Slot, child: Slot) -> Side {
        if self.child(parent, Side::Left) == Some(child) {
            Side::Left
        } else {
            assert_eq!(self.child(parent, Side::Right), Some(child));
            Side::Right
        }
    }

    fn red(&self, slot: Option<Slot>) -> bool {
        slot.is_some_and(|slot| self.links[slot.index()].color == Color::Red)
    }

    fn color(&mut self, slot: Slot, color: Color) {
        self.links[slot.index()].color = color;
    }

    /// Raise the indicated child. Only preallocated links move.
    fn rotate(&mut self, down: Slot, side: Side) {
        let up = self.child(down, side).expect("rotation child");
        let parent = self.parent(down);
        let between = self.child(up, side.other());
        let original_side = parent.map(|parent| self.side(parent, down));

        self.set_child(down, side, between);
        if let Some(between) = between {
            self.links[between.index()].parent = Some(down);
        }
        self.set_child(up, side.other(), Some(down));
        self.links[down.index()].parent = Some(up);
        self.links[up.index()].parent = parent;
        match (parent, original_side) {
            (Some(parent), Some(side)) => self.set_child(parent, side, Some(up)),
            (None, None) => self.root = Some(up),
            _ => unreachable!("rotation parent and side agree"),
        }
    }

    fn rebalance(&mut self, mut slot: Slot) {
        while let Some(parent) = self.parent(slot) {
            if !self.red(Some(parent)) {
                break;
            }
            let grand = self.parent(parent).expect("red parent has a grandparent");
            let side = self.side(grand, parent);
            let uncle = self.child(grand, side.other());
            if self.red(uncle) {
                self.color(parent, Color::Black);
                self.color(uncle.expect("red uncle"), Color::Black);
                self.color(grand, Color::Red);
                slot = grand;
                continue;
            }
            if self.child(parent, side.other()) == Some(slot) {
                slot = parent;
                self.rotate(slot, side.other());
            }
            let parent = self.parent(slot).expect("outer child");
            let grand = self.parent(parent).expect("outer grandparent");
            self.color(parent, Color::Black);
            self.color(grand, Color::Red);
            self.rotate(grand, side);
            break;
        }
        self.color(self.root.expect("inserted root"), Color::Black);
    }
}

#[cfg(test)]
#[path = "../../../../tests/metal/memory/allocator/directory.rs"]
pub(super) mod tests;

//! Fixed red-black links shared by the physical and virtual range indices.

use core::cmp::Ordering;

use crate::metal::identity::treaty::Slot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Color {
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

/// Prepaid links at a stable node position. No copied range or identity.
#[derive(Clone, Copy)]
pub(crate) struct Link {
    pub(crate) parent: Option<Slot>,
    pub(crate) left: Option<Slot>,
    pub(crate) right: Option<Slot>,
    pub(crate) color: Color,
}

impl Link {
    pub(crate) const EMPTY: Self = Self {
        parent: None,
        left: None,
        right: None,
        color: Color::Black,
    };
}

/// A checked position under the original owner's uninterrupted protection.
pub(crate) struct Insertion(Option<(Slot, Side)>);

pub(crate) struct Tree {
    root: Option<Slot>,
    links: &'static mut [Link],
    length: usize,
}

impl Tree {
    pub(crate) fn new(links: &'static mut [Link]) -> Self {
        Self {
            root: None,
            links,
            length: 0,
        }
    }

    /// Compare a proposed range with the original node at each visited slot.
    /// Equal means conflict; no business state is duplicated in the tree.
    pub(crate) fn prepare(
        &self,
        mut compare: impl FnMut(Slot) -> Ordering,
    ) -> Result<Insertion, ()> {
        let mut parent = None;
        let mut current = self.root;
        while let Some(slot) = current {
            let side = match compare(slot) {
                Ordering::Less => Side::Left,
                Ordering::Greater => Side::Right,
                Ordering::Equal => return Err(()),
            };
            parent = Some((slot, side));
            current = self.child(slot, side);
        }
        Ok(Insertion(parent))
    }

    /// Insert a prepaid original node after checking its membership and
    /// preparing the position under the same owner protection.
    pub(crate) fn insert(&mut self, slot: Slot, plan: Insertion) {
        let parent = plan.0.map(|(parent, _)| parent);
        self.links[slot.index()] = Link {
            parent,
            color: Color::Red,
            ..Link::EMPTY
        };
        match plan.0 {
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
        self.length += 1;
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

    pub(crate) fn red(&self, slot: Option<Slot>) -> bool {
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

    #[cfg(any(limen_cooperative, test))]
    pub(crate) fn is_empty(&self) -> bool {
        self.length == 0
    }

    #[cfg(any(limen_cooperative, test))]
    pub(crate) fn first(&self) -> Option<Slot> {
        let mut node = self.root?;
        while let Some(left) = self.child(node, Side::Left) {
            node = left;
        }
        Some(node)
    }

    #[cfg(any(limen_cooperative, test))]
    fn location(&self, node: Slot) -> Option<(Slot, Side)> {
        self.parent(node)
            .map(|parent| (parent, self.side(parent, node)))
    }

    #[cfg(any(limen_cooperative, test))]
    fn transplant(&mut self, old: Slot, new: Option<Slot>) {
        let parent = self.location(old);
        match parent {
            Some((parent, side)) => self.set_child(parent, side, new),
            None => self.root = new,
        }
        if let Some(new) = new {
            self.links[new.index()].parent = parent.map(|(parent, _)| parent);
        }
    }

    /// Remove an exact live member, not its business value. Move the actual
    /// successor links when it has two children; preserve all other slots.
    #[cfg(any(limen_cooperative, test))]
    pub(crate) fn remove(&mut self, node: Slot) {
        let left = self.child(node, Side::Left);
        let right = self.child(node, Side::Right);
        let mut removed_red = self.red(Some(node));
        let child;
        let hole;
        match (left, right) {
            (None, _) => {
                child = right;
                hole = self.location(node);
                self.transplant(node, child);
            }
            (Some(_), None) => {
                child = left;
                hole = self.location(node);
                self.transplant(node, child);
            }
            (Some(left), Some(right)) => {
                let mut successor = right;
                while let Some(next) = self.child(successor, Side::Left) {
                    successor = next;
                }
                removed_red = self.red(Some(successor));
                child = self.child(successor, Side::Right);
                if self.parent(successor) == Some(node) {
                    hole = Some((successor, Side::Right));
                } else {
                    hole = self.location(successor);
                    self.transplant(successor, child);
                    self.set_child(successor, Side::Right, Some(right));
                    self.links[right.index()].parent = Some(successor);
                }
                self.transplant(node, Some(successor));
                self.set_child(successor, Side::Left, Some(left));
                self.links[left.index()].parent = Some(successor);
                let color = self.links[node.index()].color;
                self.color(successor, color);
            }
        }
        self.links[node.index()] = Link::EMPTY;
        self.length -= 1;
        if !removed_red {
            self.repair(child, hole);
        }
    }

    #[cfg(any(limen_cooperative, test))]
    fn repair(&mut self, mut child: Option<Slot>, mut hole: Option<(Slot, Side)>) {
        while child != self.root && !self.red(child) {
            let (parent, side) = hole.expect("non-root black deficit");
            let mut sibling = self
                .child(parent, side.other())
                .expect("black-height sibling");
            if self.red(Some(sibling)) {
                self.color(sibling, Color::Black);
                self.color(parent, Color::Red);
                self.rotate(parent, side.other());
                sibling = self.child(parent, side.other()).expect("rotated sibling");
            }
            let near = self.child(sibling, side);
            let far = self.child(sibling, side.other());
            if !self.red(near) && !self.red(far) {
                self.color(sibling, Color::Red);
                child = Some(parent);
                hole = self.location(parent);
                continue;
            }
            if !self.red(far) {
                self.color(near.expect("red near child"), Color::Black);
                self.color(sibling, Color::Red);
                self.rotate(sibling, side);
                sibling = self.child(parent, side.other()).expect("outer sibling");
            }
            let color = self.links[parent.index()].color;
            self.color(sibling, color);
            self.color(parent, Color::Black);
            let far = self.child(sibling, side.other()).expect("red outer child");
            self.color(far, Color::Black);
            self.rotate(parent, side.other());
            return;
        }
        if let Some(child) = child {
            self.color(child, Color::Black);
        }
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> Option<Slot> {
        self.root
    }

    #[cfg(test)]
    pub(crate) fn link(&self, slot: Slot) -> &Link {
        &self.links[slot.index()]
    }
}

#[cfg(test)]
#[path = "../../tests/metal/index.rs"]
mod tests;

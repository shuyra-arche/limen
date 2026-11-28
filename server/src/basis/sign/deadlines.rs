//! Fixed Tock storage, ordered scheduled deadlines and retained due work.

use core::num::NonZeroU64;

/// One timed wait in the Sign that issued it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TockId {
    slot: usize,
    generation: NonZeroU64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Capacity,
    Gone,
    NotDue,
    AlreadyDue,
}

#[derive(Clone, Copy)]
enum Position {
    Scheduled(usize),
    Due {
        previous: Option<usize>,
        next: Option<usize>,
    },
}

struct Tock<Occupancy> {
    occupancy: Occupancy,
    deadline: u64,
    position: Position,
}

enum Slot<Occupancy> {
    Vacant {
        generation: NonZeroU64,
        next: Option<usize>,
    },
    Armed {
        generation: NonZeroU64,
        tock: Tock<Occupancy>,
    },
    Retired,
}

/// The heap and Due list share the same preallocated slots.
/// The owner provides the exact occupancy and count of its original clock.
pub(in crate::basis) struct Deadlines<Occupancy, const CAPACITY: usize> {
    slots: [Slot<Occupancy>; CAPACITY],
    unused: usize,
    free: Option<usize>,
    heap: [usize; CAPACITY],
    scheduled: usize,
    first_due: Option<usize>,
    last_due: Option<usize>,
}

impl<Occupancy: Copy + Eq, const CAPACITY: usize> Deadlines<Occupancy, CAPACITY> {
    pub(in crate::basis) const fn new() -> Self {
        Self {
            slots: [const {
                Slot::Vacant {
                    generation: NonZeroU64::MIN,
                    next: None,
                }
            }; CAPACITY],
            unused: 0,
            free: None,
            heap: [0; CAPACITY],
            scheduled: 0,
            first_due: None,
            last_due: None,
        }
    }

    pub(in crate::basis) fn arm(
        &mut self,
        occupancy: Occupancy,
        deadline: u64,
    ) -> Result<TockId, Failure> {
        let slot = match self.free {
            Some(slot) => slot,
            None if self.unused < CAPACITY => {
                let slot = self.unused;
                self.unused += 1;
                slot
            }
            None => return Err(Failure::Capacity),
        };
        let Slot::Vacant { generation, next } = self.slots[slot] else {
            unreachable!("original free deadline slot");
        };
        self.free = next;
        let position = self.scheduled;
        self.slots[slot] = Slot::Armed {
            generation,
            tock: Tock {
                occupancy,
                deadline,
                position: Position::Scheduled(position),
            },
        };
        self.heap[position] = slot;
        self.scheduled += 1;
        self.sift_up(position);
        Ok(TockId { slot, generation })
    }

    pub(in crate::basis) fn earliest(&self) -> Option<u64> {
        (self.scheduled != 0).then(|| self.tock(self.heap[0]).deadline)
    }

    /// Transfer at most the supplied number of reached deadlines into the
    /// original Due list. A comparison IRQ alone never calls this transition.
    pub(in crate::basis) fn discover_due(&mut self, lower: u64, budget: usize) -> usize {
        let mut discovered = 0;
        while discovered < budget && self.earliest().is_some_and(|deadline| deadline <= lower) {
            let slot = self.remove_scheduled(0);
            self.append_due(slot);
            discovered += 1;
        }
        discovered
    }

    pub(in crate::basis) fn due(&self) -> Option<(TockId, Occupancy, u64)> {
        let slot = self.first_due?;
        let Slot::Armed { generation, tock } = &self.slots[slot] else {
            unreachable!("original Due deadline slot");
        };
        Some((
            TockId {
                slot,
                generation: *generation,
            },
            tock.occupancy,
            tock.deadline,
        ))
    }

    pub(in crate::basis) fn check_due(
        &self,
        id: TockId,
        occupancy: Occupancy,
    ) -> Result<(), Failure> {
        match self.original(id, occupancy)? {
            Position::Due { .. } => Ok(()),
            Position::Scheduled(_) => Err(Failure::NotDue),
        }
    }

    /// Move a still-unsettled candidate behind the other Due candidates.
    pub(in crate::basis) fn rotate_due(
        &mut self,
        id: TockId,
        occupancy: Occupancy,
    ) -> Result<(), Failure> {
        self.check_due(id, occupancy)?;
        if self.last_due != Some(id.slot) {
            self.unlink_due(id.slot);
            self.append_due(id.slot);
        }
        Ok(())
    }

    /// Consume the original index registration after the Room timeout commits
    /// under the same protected transition.
    pub(in crate::basis) fn retire_due(
        &mut self,
        id: TockId,
        occupancy: Occupancy,
    ) -> Result<(), Failure> {
        self.check_due(id, occupancy)?;
        self.unlink_due(id.slot);
        self.recycle(id);
        Ok(())
    }

    /// Withdraw a scheduled Arm when its joint Room publication cannot commit.
    pub(in crate::basis) fn unarm(
        &mut self,
        id: TockId,
        occupancy: Occupancy,
    ) -> Result<(), Failure> {
        let Position::Scheduled(position) = self.original(id, occupancy)? else {
            return Err(Failure::AlreadyDue);
        };
        self.remove_scheduled(position);
        self.recycle(id);
        Ok(())
    }

    fn recycle(&mut self, id: TockId) {
        self.slots[id.slot] = match id.generation.checked_add(1) {
            Some(generation) => {
                let next = self.free.replace(id.slot);
                Slot::Vacant { generation, next }
            }
            None => Slot::Retired,
        };
    }

    fn original(&self, id: TockId, occupancy: Occupancy) -> Result<Position, Failure> {
        match self.slots.get(id.slot) {
            Some(Slot::Armed { generation, tock })
                if *generation == id.generation && tock.occupancy == occupancy =>
            {
                Ok(tock.position)
            }
            _ => Err(Failure::Gone),
        }
    }

    fn tock(&self, slot: usize) -> &Tock<Occupancy> {
        let Slot::Armed { tock, .. } = &self.slots[slot] else {
            unreachable!("original armed deadline slot");
        };
        tock
    }

    fn tock_mut(&mut self, slot: usize) -> &mut Tock<Occupancy> {
        let Slot::Armed { tock, .. } = &mut self.slots[slot] else {
            unreachable!("original armed deadline slot");
        };
        tock
    }

    fn precedes(&self, left: usize, right: usize) -> bool {
        let left = self.heap[left];
        let right = self.heap[right];
        (self.tock(left).deadline, left) < (self.tock(right).deadline, right)
    }

    fn move_scheduled(&mut self, position: usize, slot: usize) {
        self.heap[position] = slot;
        self.tock_mut(slot).position = Position::Scheduled(position);
    }

    fn swap_scheduled(&mut self, left: usize, right: usize) {
        let left_slot = self.heap[left];
        let right_slot = self.heap[right];
        self.move_scheduled(left, right_slot);
        self.move_scheduled(right, left_slot);
    }

    fn sift_up(&mut self, mut position: usize) {
        while position != 0 {
            let parent = (position - 1) / 2;
            if !self.precedes(position, parent) {
                break;
            }
            self.swap_scheduled(position, parent);
            position = parent;
        }
    }

    fn sift_down(&mut self, mut position: usize) {
        while position < self.scheduled / 2 {
            let mut child = position * 2 + 1;
            let right = child + 1;
            if right < self.scheduled && self.precedes(right, child) {
                child = right;
            }
            if !self.precedes(child, position) {
                break;
            }
            self.swap_scheduled(position, child);
            position = child;
        }
    }

    fn remove_scheduled(&mut self, position: usize) -> usize {
        let slot = self.heap[position];
        self.scheduled -= 1;
        if position < self.scheduled {
            let replacement = self.heap[self.scheduled];
            self.move_scheduled(position, replacement);
            if position != 0 && self.precedes(position, (position - 1) / 2) {
                self.sift_up(position);
            } else {
                self.sift_down(position);
            }
        }
        slot
    }

    fn append_due(&mut self, slot: usize) {
        let previous = self.last_due.replace(slot);
        self.tock_mut(slot).position = Position::Due {
            previous,
            next: None,
        };
        if let Some(previous) = previous {
            self.set_due_next(previous, Some(slot));
        } else {
            self.first_due = Some(slot);
        }
    }

    fn unlink_due(&mut self, slot: usize) {
        let Position::Due { previous, next } = self.tock(slot).position else {
            unreachable!("original Due member");
        };
        if let Some(previous) = previous {
            self.set_due_next(previous, next);
        } else {
            self.first_due = next;
        }
        if let Some(next) = next {
            let Position::Due {
                previous: former, ..
            } = &mut self.tock_mut(next).position
            else {
                unreachable!("original next Due member");
            };
            *former = previous;
        } else {
            self.last_due = previous;
        }
    }

    fn set_due_next(&mut self, slot: usize, successor: Option<usize>) {
        let Position::Due { next, .. } = &mut self.tock_mut(slot).position else {
            unreachable!("original previous Due member");
        };
        *next = successor;
    }
}

#[cfg(test)]
#[path = "../../../tests/basis/sign/deadlines.rs"]
mod tests;

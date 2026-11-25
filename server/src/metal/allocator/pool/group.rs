use core::{
    alloc::Layout,
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::MaybeUninit,
    ptr::NonNull,
};

use super::super::{Allocation, AllocationId, ReturnId};
use super::{Error, GroupId, VALUE_ALIGNMENT, VALUE_BYTES, retirement::Material};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum State {
    Free(Option<usize>),
    Reserved,
    Occupied,
    Reclaiming,
}

/// State can change under the Pool Turn while another slot's object is
/// initialized, borrowed or destroyed. Neither operation borrows the group
/// bytes as an exclusive slice after publication.
pub(super) struct Slot<T> {
    pub(super) state: Cell<State>,
    pub(super) value: UnsafeCell<MaybeUninit<T>>,
}

/// Actual per-group geometry, including the in-slot state and padding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Geometry {
    pub slots: usize,
    pub stride: usize,
    pub alignment: usize,
    pub bytes: usize,
    pub tail: usize,
}

impl Geometry {
    pub fn of<T>(bytes: usize) -> Result<Self, Error> {
        let value = Layout::new::<T>();
        if value.size() > VALUE_BYTES || value.align() > VALUE_ALIGNMENT {
            return Err(Error::Layout);
        }
        let slot = Layout::new::<Slot<T>>();
        let stride = slot.pad_to_align().size();
        let slots = bytes / stride; // The slot state gives even a ZST a nonzero stride.
        if slots == 0 {
            return Err(Error::Layout);
        }
        let layout = Layout::array::<Slot<T>>(slots).map_err(|_| Error::Layout)?;
        Ok(Self {
            slots,
            stride,
            alignment: slot.align(),
            bytes,
            tail: bytes - layout.size(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Supply {
    pub allocation: AllocationId,
    pub retirement: ReturnId,
    pub geometry: Geometry,
    pub free: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Chain {
    Empty,
    Partial,
}

#[derive(Clone, Copy)]
pub(super) struct Links {
    pub(super) previous: Option<GroupId>,
    pub(super) next: Option<GroupId>,
}

pub(super) struct Group<'storage, T> {
    // Neither the linear Allocation nor its pre-paid Return position lives
    // in the mapped group. The source can survive loss of those mapped bytes.
    pub(super) material: Material,
    pub(super) geometry: Geometry,
    slots: NonNull<Slot<T>>,
    pub(super) head: Option<usize>,
    pub(super) free: usize,
    pub(super) links: Links,
    mapping: PhantomData<&'storage mut [Slot<T>]>,
}

// The directory's Turn protects all state and free links. The pool hands out
// each value solely via its linear slot token. A newly acquired object can
// cross Harts only when T can be transferred.
unsafe impl<T: Send> Send for Group<'_, T> {}

impl<'storage, T> Group<'storage, T> {
    pub(super) fn prepare(geometry: Geometry, mapping: &mut [MaybeUninit<u8>]) {
        let slots = NonNull::from(mapping).cast::<Slot<T>>();
        for index in 0..geometry.slots {
            let next = (index + 1 < geometry.slots).then_some(index + 1);
            // Only the state field needs an initial value. An uninitialized
            // MaybeUninit<T> is valid, and no group-size or T-size temporary
            // needs to occupy the bootstrap stack.
            unsafe {
                (&raw mut (*slots.as_ptr().add(index)).state).write(Cell::new(State::Free(next)));
            }
        }
    }

    pub(super) fn new(
        allocation: Allocation,
        retirement: ReturnId,
        geometry: Geometry,
        mapping: &'storage mut [MaybeUninit<u8>],
    ) -> Self {
        let slots = NonNull::from(mapping).cast::<Slot<T>>();
        Self {
            material: Material {
                allocation,
                retirement,
            },
            geometry,
            slots,
            head: Some(0),
            free: geometry.slots,
            links: Links {
                previous: None,
                next: None,
            },
            mapping: PhantomData,
        }
    }

    pub(super) fn supply(&self) -> Supply {
        Supply {
            allocation: self.material.allocation.id(),
            retirement: self.material.retirement,
            geometry: self.geometry,
            free: self.free,
        }
    }

    pub(super) fn chain(&self) -> Option<Chain> {
        if self.free == 0 {
            None
        } else if self.free == self.geometry.slots {
            Some(Chain::Empty)
        } else {
            Some(Chain::Partial)
        }
    }

    pub(super) fn slot(&self, index: usize) -> NonNull<Slot<T>> {
        assert!(index < self.geometry.slots);
        // The exact prefix was initialized and the directory retains its mapping.
        unsafe { self.slots.add(index) }
    }
}

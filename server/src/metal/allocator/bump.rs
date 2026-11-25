//! Checked bootstrap allocation within one already admitted interval.

use core::{alloc::Layout, ops::Range};

/// A monotonic reservation cursor. The caller retains custody of the backing
/// storage and excludes prior allocations, firmware reservations and holes.
pub(crate) struct Bump {
    frontier: usize,
    edge: usize,
}

impl Bump {
    pub(crate) fn new(available: Range<usize>) -> Option<Self> {
        (!available.is_empty()).then_some(Self {
            frontier: available.start,
            edge: available.end,
        })
    }

    pub(crate) fn reserve(&mut self, layout: Layout) -> Option<Range<usize>> {
        let start = self.frontier.checked_next_multiple_of(layout.align())?;
        let end = start.checked_add(layout.size())?;
        if end > self.edge {
            return None;
        }
        self.frontier = end;
        Some(start..end)
    }
}

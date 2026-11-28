//! Start and close one original metered occupancy.

use super::{DurationBounds, LocalTiming, Sample};

#[derive(Clone, Copy)]
pub(crate) struct Meter<'clock, Occupancy> {
    occupancy: Occupancy,
    start: Sample<'clock>,
}

#[derive(Clone, Copy)]
pub(crate) struct MeterReceipt<Occupancy> {
    occupancy: Occupancy,
    evidence: Evidence,
}

#[derive(Clone, Copy)]
enum Evidence {
    Unused,
    Measured(DurationBounds),
    Uncertain,
}

impl<'clock, Occupancy> Meter<'clock, Occupancy> {
    pub(crate) fn start(occupancy: Occupancy, sample: Sample<'clock>) -> Self {
        Self {
            occupancy,
            start: sample,
        }
    }

    pub(crate) fn close(
        self,
        timing: &LocalTiming<'clock>,
        sample: Option<Sample<'clock>>,
    ) -> MeterReceipt<Occupancy> {
        let evidence = sample
            .and_then(|end| timing.measure(self.start, end))
            .map_or(Evidence::Uncertain, Evidence::Measured);
        MeterReceipt {
            occupancy: self.occupancy,
            evidence,
        }
    }
}

impl<Occupancy: Copy> MeterReceipt<Occupancy> {
    pub(crate) fn unused(occupancy: Occupancy) -> Self {
        Self {
            occupancy,
            evidence: Evidence::Unused,
        }
    }

    pub(crate) fn is_unused(self) -> bool {
        matches!(self.evidence, Evidence::Unused)
    }

    pub(crate) fn occupancy(self) -> Occupancy {
        self.occupancy
    }

    pub(crate) fn evidence(self) -> Option<DurationBounds> {
        match self.evidence {
            Evidence::Unused => Some(DurationBounds {
                minimum: core::time::Duration::ZERO,
                maximum: core::time::Duration::ZERO,
            }),
            Evidence::Measured(bounds) => Some(bounds),
            Evidence::Uncertain => None,
        }
    }
}

use limen_treaty::model::{Alignment, ByteLength, PhysicalRange};

use super::super::region::buddy;
use super::{Error, RegionDescription, RegionId, RegionOrigin};

/// The recipient's intended use, independent of the physical range selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RamPurpose {
    Metadata,
    Execution,
    PageTable,
    Cell,
}

/// Fixed purpose, layout and physical eligibility for one lookup. Actual
/// spending still revalidates these facts under the authorization's protection.
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub(in crate::metal::memory::allocator) buddy: buddy::Request,
    attributes: Option<RegionOrigin>,
    purpose: RamPurpose,
}

impl Request {
    pub fn new(
        length: ByteLength,
        alignment: Alignment,
        window: PhysicalRange,
        attributes: Option<RegionOrigin>,
        purpose: RamPurpose,
    ) -> Result<Self, Error> {
        let buddy = buddy::Request::new(length, alignment)
            .map_err(|_| Error::Layout)?
            .restrict(window);
        Ok(Self {
            buddy,
            attributes,
            purpose,
        })
    }

    pub fn length(self) -> ByteLength {
        self.buddy.length()
    }

    pub fn alignment(self) -> Alignment {
        self.buddy.alignment()
    }

    pub fn purpose(self) -> RamPurpose {
        self.purpose
    }

    pub(in crate::metal::memory::allocator) fn accepts(self, region: RegionDescription) -> bool {
        self.buddy
            .window()
            .is_some_and(|window| window.overlaps(region.physical))
            && self
                .attributes
                .is_none_or(|attributes| attributes == region.origin)
    }
}

/// Physical observation only. The candidate is rechecked during occupation.
#[derive(Clone, Copy)]
pub struct Candidate {
    pub(in crate::metal::memory::allocator) region: RegionId,
    pub(in crate::metal::memory::allocator) physical: PhysicalRange,
    pub(in crate::metal::memory::allocator) request: Request,
}

impl Candidate {
    pub fn physical(self) -> PhysicalRange {
        self.physical
    }

    pub fn region(self) -> RegionId {
        self.region
    }

    pub fn request(self) -> Request {
        self.request
    }
}

pub enum Search {
    Pending,
    Candidate(Candidate),
    NoRange,
}

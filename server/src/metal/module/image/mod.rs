//! Checks and prepares independently linked kernel module images.

mod description;
mod elf;
pub(super) mod placement;

use super::treaty::Contract;
pub use description::{Description, DescriptionError};
pub use elf::{Machine, SegmentPermissions};

use limen_treaty::model::{ByteCoordinate, ByteLength, PageSize, Range};

/// Coordinates in the link-time image, not in an installed address space.
pub enum Image {}
pub type ImageAddress = ByteCoordinate<Image>;
type ImageRange = Range<ImageAddress>;

// Fixed provider-selection work bound over input supplied by the actual source.
const MAX_IMAGES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Format,
    Unsupported,
    Capacity,
    Range,
    Conflict,
    Description(DescriptionError),
}

/// A checked, immutable image and the metadata read from that same image.
#[derive(Clone)]
pub struct ModuleImage<'a> {
    elf: elf::Elf<'a>,
    description: Description<'a>,
    page_size: PageSize,
    machine: Machine,
}

impl<'a> ModuleImage<'a> {
    /// Interpret the file within its independently supplied byte extent.
    /// The caller establishes the source's trust and supplies the target
    /// machine and actual initial page granule.
    pub fn read(bytes: &'a [u8], machine: Machine, page_size: PageSize) -> Result<Self, Error> {
        let elf = elf::Elf::read(bytes, machine, page_size)?;
        let description = Description::read(&elf).map_err(Error::Description)?;
        Ok(Self {
            elf,
            description,
            page_size,
            machine,
        })
    }

    /// Select a unique provider from the retained images for this target.
    /// Each image carries its own declaration. The source supplies its own
    /// bounded bytes and establishes their provenance and lifetime.
    pub fn select(
        source: &[&'a [u8]],
        machine: Machine,
        page_size: PageSize,
        contract: Contract<'_>,
    ) -> Result<Option<Self>, Error> {
        if source.len() > MAX_IMAGES {
            return Err(Error::Capacity);
        }
        let mut selected = None;
        for bytes in source {
            let image = Self::read(bytes, machine, page_size)?;
            if image.description.interface(contract).is_some() && selected.replace(image).is_some()
            {
                return Err(Error::Conflict);
            }
        }
        Ok(selected)
    }

    pub fn description(&self) -> Description<'a> {
        self.description
    }

    pub fn required_length(&self) -> ByteLength {
        self.elf.span().length()
    }
}

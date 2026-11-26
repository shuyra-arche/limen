//! Checks and prepares independently linked kernel module images.

mod description;
mod elf;
mod lifetime;

pub use description::{Contract, Description, DescriptionError, Interface};
pub use elf::{Machine, Permissions};
pub use lifetime::{Module, ModuleId, ModuleSlots, ModuleUse, Retirement, UseError};

use crate::metal::mmu::internal::access::MappedRange;
use core::mem::MaybeUninit;
use limen_treaty::model::{
    Alignment, ByteCoordinate, ByteLength, InternalAddress, InternalRange, PageSize, PhysicalRange,
    Range,
};

/// Coordinates in the link-time image, not in an installed address space.
pub enum Image {}
pub type ImageAddress = ByteCoordinate<Image>;
type ImageRange = Range<ImageAddress>;

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

    /// Copy and relocate into a private mapped range of exactly the required
    /// length. The storage owner establishes its initial writable, non-
    /// executable protection and keeps custody after this operation.
    ///
    /// A placement failure leaves the original bytes untouched. The layout
    /// describes the bytes and intended permissions for the supplied Internal
    /// start. Before calling any entry, the mapping owner must publish the
    /// permissions, close conflicting aliases and establish instruction
    /// visibility for the target Hart.
    pub fn prepare(
        &self,
        storage: &mut MappedRange<'_>,
        internal_start: InternalAddress,
    ) -> Result<Layout<'a>, Error> {
        let span = self.elf.span();
        let length = self.required_length();
        let physical = storage.physical();
        let page = Alignment::new(self.page_size.get()).expect("page alignment");
        let alignment = self.elf.alignment().get();
        let internal = internal_start.range(length).ok_or(Error::Range)?;
        if length.as_usize() != Some(storage.length())
            || !physical.start().is_aligned(page)
            || internal_start.get() % alignment != span.start().get() % alignment
        {
            return Err(Error::Range);
        }

        // The bounded ELF plan establishes every file and placement offset
        // within this exact range before any write takes place.
        let bytes = storage.bytes();
        bytes.fill(MaybeUninit::new(0));
        for segment in self.elf.segments() {
            let offset = segment
                .address
                .distance(span.start())
                .and_then(ByteLength::as_usize)
                .expect("checked segment offset");
            for (target, source) in bytes[offset..offset + segment.file.len()]
                .iter_mut()
                .zip(segment.file)
            {
                target.write(*source);
            }
        }
        for relocation in self.elf.relocations() {
            let offset = relocation
                .address
                .distance(span.start())
                .and_then(ByteLength::as_usize)
                .expect("checked relocation offset");
            let value = internal_start
                .advance(
                    relocation
                        .addend
                        .distance(span.start())
                        .expect("checked addend"),
                )
                .expect("checked placement")
                .get();
            for (target, source) in bytes[offset..offset + 8]
                .iter_mut()
                .zip(value.to_le_bytes())
            {
                target.write(source);
            }
        }

        Ok(Layout {
            image: self.clone(),
            physical,
            internal,
        })
    }
}

/// The placement of prepared bytes. The backing range remains with its owner.
pub struct Layout<'source> {
    image: ModuleImage<'source>,
    physical: PhysicalRange,
    internal: InternalRange,
}

impl Layout<'_> {
    pub fn machine(&self) -> Machine {
        self.image.machine
    }

    pub fn physical(&self) -> PhysicalRange {
        self.physical
    }

    pub fn internal(&self) -> InternalRange {
        self.internal
    }

    /// Page-sized permission ranges. Gaps in the layout have no entry here.
    pub fn segments(&self) -> impl Iterator<Item = PlacedSegment> + '_ {
        let alignment = Alignment::new(self.image.page_size.get()).expect("page alignment");
        let start = self.image.elf.span().start();
        self.image.elf.segments().map(move |segment| {
            let first = segment
                .address
                .align_down(alignment)
                .distance(start)
                .expect("checked segment start");
            let last = segment
                .address
                .advance(segment.memory_size)
                .and_then(|end| end.align_up(alignment))
                .and_then(|end| end.distance(start))
                .expect("checked segment end");
            let length = last.checked_sub(first).expect("checked segment length");
            let physical = self
                .physical
                .start()
                .advance(first)
                .and_then(|start| start.range(length))
                .expect("segment is within the physical placement");
            let internal = self
                .internal
                .start()
                .advance(first)
                .and_then(|start| start.range(length))
                .expect("segment is within the Internal placement");
            PlacedSegment {
                physical,
                internal,
                permissions: segment.permissions,
            }
        })
    }

    /// An Internal entry coordinate for the exact interface in this image.
    pub fn entry(&self, contract: Contract<'_>) -> Option<InternalAddress> {
        let interface = self.image.description.interface(contract)?;
        Some(self.place_entry(interface))
    }

    fn place_entry(&self, interface: Interface<'_>) -> InternalAddress {
        let offset = interface
            .entry()
            .distance(self.image.elf.span().start())
            .expect("checked interface offset");
        self.internal
            .start()
            .advance(offset)
            .expect("checked placement")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacedSegment {
    pub physical: PhysicalRange,
    pub internal: InternalRange,
    pub permissions: Permissions,
}

//! Checks and prepares independently linked kernel module images.

mod description;
mod elf;
mod lifetime;

pub use description::{Contract, Description, DescriptionError, Interface};
pub use elf::{Machine, Permissions};
pub use lifetime::{Module, ModuleId, ModuleSlots, ModuleUse, Retirement, UseError};

use crate::metal::{
    address::{PhysicalAddress, PhysicalRange},
    mmu::internal::access::MappedRange,
};
use core::{mem::MaybeUninit, ops::Range};
use limen_treaty::model::{ByteLength, PageSize};

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
        let elf = elf::Elf::read(bytes, machine, page_size.get())?;
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
        let span = self.elf.span();
        ByteLength::new(span.end - span.start)
    }

    /// Copy and relocate into a private mapped range of exactly the required
    /// length. The storage owner establishes its initial writable, non-
    /// executable protection and keeps custody after this operation.
    ///
    /// A placement failure leaves the original bytes untouched. The layout
    /// describes the bytes and intended permissions for the supplied virtual
    /// start. Before calling any entry, the mapping owner must publish the
    /// permissions, close conflicting aliases and establish instruction
    /// visibility for the target Hart.
    pub fn prepare(
        &self,
        storage: &mut MappedRange<'_>,
        virtual_start: u64,
    ) -> Result<Layout<'a>, Error> {
        let span = self.elf.span();
        let length = self.required_length();
        let physical = storage.physical();
        let page = self.page_size.get();
        let alignment = self.elf.alignment();
        let end = virtual_start
            .checked_add(length.get())
            .ok_or(Error::Range)?;
        if length.as_usize() != Some(storage.length())
            || !physical.start().get().is_multiple_of(page)
            || virtual_start % alignment != span.start % alignment
        {
            return Err(Error::Range);
        }

        // The bounded ELF plan establishes every file and placement offset
        // within this exact range before any write takes place.
        let bytes = storage.bytes();
        bytes.fill(MaybeUninit::new(0));
        for segment in self.elf.segments() {
            let offset = (segment.address - span.start) as usize;
            for (target, source) in bytes[offset..offset + segment.file.len()]
                .iter_mut()
                .zip(segment.file)
            {
                target.write(*source);
            }
        }
        for relocation in self.elf.relocations() {
            let offset = (relocation.address - span.start) as usize;
            let value = virtual_start + (relocation.addend - span.start);
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
            virtual_range: virtual_start..end,
        })
    }
}

/// The placement of prepared bytes. The backing range remains with its owner.
pub struct Layout<'source> {
    image: ModuleImage<'source>,
    physical: PhysicalRange,
    virtual_range: Range<u64>,
}

impl Layout<'_> {
    pub fn machine(&self) -> Machine {
        self.image.machine
    }

    pub fn physical(&self) -> PhysicalRange {
        self.physical
    }

    pub fn virtual_range(&self) -> Range<u64> {
        self.virtual_range.clone()
    }

    /// Page-sized permission ranges. Gaps in the layout have no entry here.
    pub fn segments(&self) -> impl Iterator<Item = PlacedSegment> + '_ {
        let page = self.image.page_size.get();
        let start = self.image.elf.span().start;
        self.image.elf.segments().map(move |segment| {
            let first = segment.address - segment.address % page - start;
            let segment_end = segment.address + segment.memory_size;
            let last = segment_end.next_multiple_of(page) - start;
            let physical = PhysicalRange::new(
                PhysicalAddress::new(self.physical.start().get() + first),
                PhysicalAddress::new(self.physical.start().get() + last),
            )
            .expect("ELF segment is contained in the checked placement");
            PlacedSegment {
                physical,
                virtual_range: self.virtual_range.start + first..self.virtual_range.start + last,
                permissions: segment.permissions,
            }
        })
    }

    /// A numeric entry location for the exact interface in this image.
    pub fn entry(&self, contract: Contract<'_>) -> Option<u64> {
        let interface = self.image.description.interface(contract)?;
        Some(self.place_entry(interface))
    }

    fn place_entry(&self, interface: Interface<'_>) -> u64 {
        self.virtual_range.start + (interface.entry() - self.image.elf.span().start)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacedSegment {
    pub physical: PhysicalRange,
    pub virtual_range: Range<u64>,
    pub permissions: Permissions,
}

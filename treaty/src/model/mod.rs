//! Checked coordinates and geometry shared by Treaty consumers.

mod address;
mod geometry;

pub use address::{
    ByteCoordinate, ByteOffset, External, ExternalAddress, Internal, InternalAddress, PageOffset,
    Physical, PhysicalAddress, Relative,
};
pub use geometry::{Alignment, ByteLength, PageCount, PageSize, Range};

pub type PhysicalRange = Range<PhysicalAddress>;
pub type InternalRange = Range<InternalAddress>;
pub type ExternalRange = Range<ExternalAddress>;
pub type ByteRange = Range<ByteOffset>;
pub type PageRange = Range<PageOffset>;

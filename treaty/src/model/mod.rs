//! Checked coordinates and geometry shared by Treaty consumers.

mod address;
mod geometry;

pub use address::{ByteOffset, ExternalAddress, InternalAddress, PageOffset};
pub use geometry::{Alignment, ByteLength, PageCount, PageSize, Range};

pub type InternalRange = Range<InternalAddress>;
pub type ExternalRange = Range<ExternalAddress>;
pub type ByteRange = Range<ByteOffset>;
pub type PageRange = Range<PageOffset>;

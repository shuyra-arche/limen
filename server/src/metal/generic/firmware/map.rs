//! Checked physical descriptions from the final UEFI memory map.
//! A descriptor is a firmware fact, not permission to allocate or access RAM.

use core::ops::Range;

const PREFIX: usize = 40;
const ALIGNMENT: usize = 8;
const PAGE_BYTES: u64 = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    Format,
    Capacity,
    Range,
    Overlap,
}

/// A v1 descriptor's physical interval and original classification.
/// Attribute bits describe firmware-reported capabilities, not an active mapping.
#[derive(Debug, PartialEq, Eq)]
pub struct FirmwareRegion {
    pub kind: u32,
    pub physical: Range<u64>,
    pub attributes: u64,
}

/// A checked, immutable view of firmware descriptor bytes. Constructing a view
/// neither grants custody nor establishes retirement of old CPU/device accesses.
#[derive(Clone, Copy)]
pub struct FirmwareMap<'a> {
    descriptors: &'a [u8],
    stride: usize,
    version: u32,
}

impl<'a> FirmwareMap<'a> {
    /// Validate all descriptors before exposing any region. The trusted loader
    /// supplies its configured descriptor limit; no allocation or sorting is
    /// needed. Unsorted maps are accepted, but overlapping regions are not.
    /// The pairwise check uses at most limit * (limit - 1) / 2 comparisons.
    pub fn new(
        descriptors: &'a [u8],
        stride: usize,
        version: u32,
        limit: usize,
    ) -> Result<Self, MapError> {
        if version != 1
            || stride < PREFIX
            || !stride.is_multiple_of(ALIGNMENT)
            || descriptors.is_empty()
            || !descriptors.as_ptr().addr().is_multiple_of(ALIGNMENT)
            || !descriptors.len().is_multiple_of(stride)
        {
            return Err(MapError::Format);
        }

        let entries = descriptors.chunks_exact(stride);
        if entries.len() > limit {
            return Err(MapError::Capacity);
        }
        for (index, entry) in entries.clone().enumerate() {
            let region = decode(entry).ok_or(MapError::Range)?;
            for earlier in entries.clone().take(index) {
                let other = decode(earlier).ok_or(MapError::Range)?;
                if region.physical.start < other.physical.end
                    && other.physical.start < region.physical.end
                {
                    return Err(MapError::Overlap);
                }
            }
        }
        Ok(Self {
            descriptors,
            stride,
            version,
        })
    }

    pub fn descriptors(&self) -> &'a [u8] {
        self.descriptors
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn regions(&self) -> impl ExactSizeIterator<Item = FirmwareRegion> + '_ {
        // The bytes are immutable and every prefix was checked at construction.
        self.descriptors
            .chunks_exact(self.stride)
            .map(|entry| decode(entry).expect("validated firmware descriptor"))
    }
}

fn decode(entry: &[u8]) -> Option<FirmwareRegion> {
    let kind = u32::from_le_bytes(entry[..4].try_into().unwrap());
    let start = u64::from_le_bytes(entry[8..16].try_into().unwrap());
    let pages = u64::from_le_bytes(entry[24..32].try_into().unwrap());
    let attributes = u64::from_le_bytes(entry[32..40].try_into().unwrap());
    if !start.is_multiple_of(PAGE_BYTES) || pages == 0 {
        return None;
    }
    let end = start.checked_add(pages.checked_mul(PAGE_BYTES)?)?;
    Some(FirmwareRegion {
        kind,
        physical: start..end,
        attributes,
    })
}

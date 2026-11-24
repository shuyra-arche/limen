use core::ops::Range;

const ALIGNMENT: usize = 8;
const HEADER_SIZE: usize = 40;
const MAGIC: u32 = 0xd00d_feed;

/// # Safety
///
/// The trusted firmware must supply immutable readable storage for the header
/// at a nonzero aligned address. This check reads only the fixed-size header.
pub(super) unsafe fn extent(address: usize, image: Range<usize>) -> Option<Range<usize>> {
    let header_end = address.checked_add(HEADER_SIZE)?;
    if address == 0 || !address.is_multiple_of(ALIGNMENT) || overlaps(address..header_end, &image) {
        return None;
    }

    let header = unsafe { core::ptr::without_provenance::<[u32; 10]>(address).read_volatile() };
    let [
        magic,
        size,
        structure,
        strings,
        reservations,
        version,
        compatible,
        _,
        strings_size,
        structure_size,
    ] = header.map(u32::from_be);
    if magic != MAGIC || version < 17 || compatible > 17 || compatible > version {
        return None;
    }
    if !block(structure, structure_size, size, 4)
        || !block(strings, strings_size, size, 1)
        || !block(reservations, 16, size, ALIGNMENT as u32)
    {
        return None;
    }
    let extent = address..address.checked_add(size as usize)?;
    (!overlaps(extent.clone(), &image)).then_some(extent)
}

fn block(offset: u32, length: u32, size: u32, alignment: u32) -> bool {
    offset >= HEADER_SIZE as u32
        && offset.is_multiple_of(alignment)
        && offset.checked_add(length).is_some_and(|end| end <= size)
}

fn overlaps(first: Range<usize>, second: &Range<usize>) -> bool {
    first.start < second.end && second.start < first.end
}

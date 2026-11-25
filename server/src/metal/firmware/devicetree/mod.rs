use core::ops::Range;

pub(crate) mod memory;

const ALIGNMENT: usize = 8;
pub(super) const HEADER_SIZE: usize = 40;
const MAGIC: u32 = 0xd00d_feed;

/// # Safety
///
/// The trusted firmware must supply immutable readable storage for the header
/// at a nonzero aligned address. This check reads only the fixed-size header.
pub(crate) unsafe fn extent(header: *const [u32; 10], image: Range<usize>) -> Option<Range<usize>> {
    let address = header.addr();
    let header_end = address.checked_add(HEADER_SIZE)?;
    if header.is_null()
        || !address.is_multiple_of(ALIGNMENT)
        || overlaps(&(address..header_end), &image)
    {
        return None;
    }

    let header = layout(unsafe { header.read_volatile() }.map(u32::from_be))?;
    let extent = address..address.checked_add(header.size as usize)?;
    (!overlaps(&extent, &image)).then_some(extent)
}

struct Layout {
    size: u32,
    structure: Range<u32>,
    strings: Range<u32>,
    reservations: u32,
}

fn layout(header: [u32; 10]) -> Option<Layout> {
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
    ] = header;
    if magic != MAGIC || version < 17 || compatible > 17 || compatible > version {
        return None;
    }
    // Even an empty root needs BEGIN_NODE, its padded name, END_NODE and END.
    let structure = block(structure, structure_size, size, 4)?;
    let strings = block(strings, strings_size, size, 1)?;
    // Only the first reservation entry is knowable from the header.
    let reservations = block(reservations, 16, size, ALIGNMENT as u32)?;
    if structure_size < 16
        || !structure_size.is_multiple_of(4)
        || overlaps(&structure, &strings)
        || overlaps(&structure, &reservations)
        || overlaps(&strings, &reservations)
    {
        return None;
    }
    Some(Layout {
        size,
        structure,
        strings,
        reservations: reservations.start,
    })
}

fn block(offset: u32, length: u32, size: u32, alignment: u32) -> Option<Range<u32>> {
    let end = offset.checked_add(length)?;
    (offset >= HEADER_SIZE as u32 && offset.is_multiple_of(alignment) && end <= size)
        .then_some(offset..end)
}

fn overlaps<T: Ord>(first: &Range<T>, second: &Range<T>) -> bool {
    !first.is_empty() && !second.is_empty() && first.start < second.end && second.start < first.end
}

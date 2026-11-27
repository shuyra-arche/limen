//! Bounded views of a flattened device tree blob.

use core::{fmt, ops::Range};

pub(crate) const ALIGNMENT: usize = 8;
pub(crate) const HEADER_SIZE: usize = 40;
const MAGIC: u32 = 0xd00d_feed;
pub(crate) const MAX_BLOCK_BYTES: usize = 256 * 1024;
// AAVMF retains the QEMU tree with free space and an extra EFI page.
const MAX_EXTENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 4096;
const MAX_PROPERTIES: usize = 64;
pub(crate) const RESERVATION_LIMIT: usize = 64;

const BEGIN: u32 = 1;
const END_NODE: u32 = 2;
const PROPERTY: u32 = 3;
const NOP: u32 = 4;
const END: u32 = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Window,
    Format,
    Range,
    Capacity,
    Unsupported,
    Conflict,
    Missing,
}

pub(crate) struct HeaderLayout {
    pub(crate) size: u32,
    structure: Range<u32>,
    strings: Range<u32>,
    reservations: u32,
}

pub(crate) fn header_layout(header: [u32; 10]) -> Option<HeaderLayout> {
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
    let structure = block(structure, structure_size, size, 4)?;
    let strings = block(strings, strings_size, size, 1)?;
    let reservations = block(reservations, 16, size, ALIGNMENT as u32)?;
    if structure_size < 16
        || !structure_size.is_multiple_of(4)
        || overlaps(&structure, &strings)
        || overlaps(&structure, &reservations)
        || overlaps(&strings, &reservations)
    {
        return None;
    }
    Some(HeaderLayout {
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

/// A structurally validated, bounded view of one immutable flattened tree.
#[derive(Clone, Copy)]
pub struct DeviceTree<'a> {
    bytes: &'a [u8],
    structure: &'a [u8],
    structure_offset: usize,
    strings: &'a [u8],
    reservations: &'a [u8],
}

impl fmt::Debug for DeviceTree<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceTree")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl<'a> DeviceTree<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_EXTENT_BYTES {
            return Err(Error::Capacity);
        }
        let header = bytes.first_chunk::<HEADER_SIZE>().ok_or(Error::Format)?;
        let mut words = [0; 10];
        for (word, raw) in words.iter_mut().zip(header.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*raw);
        }
        let layout = header_layout(words).ok_or(Error::Format)?;
        if layout.size as usize != bytes.len() {
            return Err(Error::Format);
        }
        if layout.structure.len() + layout.strings.len() > MAX_BLOCK_BYTES {
            return Err(Error::Capacity);
        }

        let start = layout.reservations;
        let edge = [&layout.structure, &layout.strings]
            .into_iter()
            .filter(|block| !block.is_empty() && block.start > start)
            .map(|block| block.start)
            .min()
            .unwrap_or(layout.size);
        let reservation_window = &bytes[start as usize..edge as usize];
        let mut reservations = None;
        for (index, entry) in reservation_window
            .as_chunks::<16>()
            .0
            .iter()
            .take(RESERVATION_LIMIT + 1)
            .enumerate()
        {
            if *entry == [0; 16] {
                reservations = Some(&reservation_window[..index * 16]);
                break;
            }
        }
        let reservations = reservations.ok_or({
            if reservation_window.len() >= (RESERVATION_LIMIT + 1) * 16 {
                Error::Capacity
            } else {
                Error::Format
            }
        })?;

        let tree = Self {
            bytes,
            structure: &bytes[layout.structure.start as usize..layout.structure.end as usize],
            structure_offset: layout.structure.start as usize,
            strings: &bytes[layout.strings.start as usize..layout.strings.end as usize],
            reservations,
        };
        let mut nodes = tree.nodes();
        let mut count = 0;
        while let Some(node) = nodes.next_node()? {
            count += 1;
            if count > MAX_NODES {
                return Err(Error::Capacity);
            }
            node.validate_properties()?;
        }
        Ok(tree)
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn nodes(&self) -> Nodes<'_, 'a> {
        Nodes {
            tree: self,
            cursor: Cursor {
                bytes: self.structure,
                offset: self.structure_offset,
            },
            parents: [0; MAX_DEPTH],
            depth: 0,
            root_seen: false,
            ended: false,
        }
    }

    pub fn node(&self, id: NodeId<'a>) -> Option<Node<'_, 'a>> {
        if !core::ptr::eq(self.bytes, id.source) {
            return None;
        }
        self.nodes().find(|node| node.id == id)
    }

    /// Locate an absolute path in this tree. Alias interpretation belongs to the caller.
    pub fn find_path(&self, path: &[u8]) -> Result<Option<Node<'_, 'a>>, Error> {
        let path = path.strip_prefix(b"/").ok_or(Error::Format)?;
        let mut node = self.nodes().next().expect("validated device tree root");
        if path.is_empty() {
            return Ok(Some(node));
        }
        for (depth, name) in path.split(|byte| *byte == b'/').enumerate() {
            if depth + 1 >= MAX_DEPTH {
                return Err(Error::Capacity);
            }
            if name.is_empty() {
                return Err(Error::Format);
            }
            let mut children = self
                .nodes()
                .filter(|child| child.parent == Some(node.id) && child.name == name);
            let Some(child) = children.next() else {
                return Ok(None);
            };
            if children.next().is_some() {
                return Err(Error::Conflict);
            }
            node = child;
        }
        Ok(Some(node))
    }

    /// Resolve a phandle within this source and reject ambiguous references.
    pub fn phandle(&self, value: u32) -> Result<Option<Node<'_, 'a>>, Error> {
        if value == 0 || value == u32::MAX {
            return Err(Error::Format);
        }
        let mut found = None;
        for node in self.nodes() {
            if node.phandle()? == Some(value) && found.replace(node).is_some() {
                return Err(Error::Conflict);
            }
        }
        Ok(found)
    }

    pub(super) fn reservations(&self) -> impl Iterator<Item = (u64, u64)> + 'a {
        self.reservations.as_chunks::<16>().0.iter().map(|entry| {
            let (base, length) = entry.split_first_chunk::<8>().expect("reservation entry");
            (
                u64::from_be_bytes(*base),
                u64::from_be_bytes(*length.first_chunk::<8>().expect("reservation length")),
            )
        })
    }
}

/// The byte position of a node within its original immutable source.
#[derive(Clone, Copy)]
pub struct NodeId<'a> {
    source: &'a [u8],
    offset: usize,
}

impl NodeId<'_> {
    pub fn offset(self) -> usize {
        self.offset
    }
}

impl fmt::Debug for NodeId<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("NodeId").field(&self.offset).finish()
    }
}

impl PartialEq for NodeId<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset && core::ptr::eq(self.source, other.source)
    }
}

impl Eq for NodeId<'_> {}

#[derive(Clone, Copy)]
pub struct Node<'tree, 'a> {
    tree: &'tree DeviceTree<'a>,
    id: NodeId<'a>,
    parent: Option<NodeId<'a>>,
    name: &'a [u8],
    properties: Cursor<'a>,
}

impl fmt::Debug for Node<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Node")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl<'a> Node<'_, 'a> {
    pub fn id(&self) -> NodeId<'a> {
        self.id
    }

    pub fn parent_id(&self) -> Option<NodeId<'a>> {
        self.parent
    }

    pub fn name(&self) -> &'a [u8] {
        self.name
    }

    pub fn property(&self, name: &[u8]) -> Result<Option<&'a [u8]>, Error> {
        Ok(self.property_entry(name)?.map(Property::value))
    }

    pub fn property_entry(&self, name: &[u8]) -> Result<Option<Property<'a>>, Error> {
        let mut cursor = self.properties;
        while let Some(property) = cursor.property(self.tree.strings)? {
            if property.name == name {
                return Ok(Some(property));
            }
        }
        Ok(None)
    }

    pub fn string(&self, name: &[u8]) -> Result<Option<&'a [u8]>, Error> {
        let Some(bytes) = self.property(name)? else {
            return Ok(None);
        };
        let value = bytes
            .strip_suffix(b"\0")
            .filter(|value| !value.is_empty() && printable(value))
            .ok_or(Error::Format)?;
        Ok(Some(value))
    }

    pub(crate) fn parent(&self) -> Result<Option<Self>, Error> {
        self.parent
            .map(|id| self.tree.node(id).ok_or(Error::Missing))
            .transpose()
    }

    fn validate_properties(&self) -> Result<(), Error> {
        let mut names = [b"".as_slice(); MAX_PROPERTIES];
        let mut count = 0;
        let mut cursor = self.properties;
        while let Some(property) = cursor.property(self.tree.strings)? {
            if names[..count].contains(&property.name) {
                return Err(Error::Conflict);
            }
            *names.get_mut(count).ok_or(Error::Capacity)? = property.name;
            count += 1;
        }
        self.phandle()?;
        Ok(())
    }

    fn phandle(&self) -> Result<Option<u32>, Error> {
        let decode = |bytes: &[u8]| {
            let word = <[u8; 4]>::try_from(bytes).map_err(|_| Error::Format)?;
            let value = u32::from_be_bytes(word);
            if value == 0 || value == u32::MAX {
                return Err(Error::Format);
            }
            Ok(value)
        };
        let standard = self.property(b"phandle")?.map(decode).transpose()?;
        let legacy = self.property(b"linux,phandle")?.map(decode).transpose()?;
        if let (Some(standard), Some(legacy)) = (standard, legacy)
            && standard != legacy
        {
            return Err(Error::Conflict);
        }
        Ok(standard.or(legacy))
    }
}

pub(crate) fn printable(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| (b' '..=b'~').contains(byte))
}

#[derive(Clone, Copy, Debug)]
pub struct Property<'a> {
    name: &'a [u8],
    value: &'a [u8],
    offset: usize,
}

impl<'a> Property<'a> {
    /// Byte position of the property token within its source blob.
    pub fn offset(self) -> usize {
        self.offset
    }

    pub fn value(self) -> &'a [u8] {
        self.value
    }
}

pub struct Nodes<'tree, 'a> {
    tree: &'tree DeviceTree<'a>,
    cursor: Cursor<'a>,
    parents: [usize; MAX_DEPTH],
    depth: usize,
    root_seen: bool,
    ended: bool,
}

impl<'tree, 'a> Nodes<'tree, 'a> {
    fn next_node(&mut self) -> Result<Option<Node<'tree, 'a>>, Error> {
        while !self.ended {
            let offset = self.cursor.offset;
            match self.cursor.word()? {
                NOP => (),
                BEGIN => {
                    let name = self.cursor.name()?;
                    if name.is_empty() == self.root_seen
                        || (self.root_seen && self.depth == 0)
                        || !printable(name)
                        || name.contains(&b'/')
                    {
                        return Err(Error::Format);
                    }
                    let id = NodeId {
                        source: self.tree.bytes,
                        offset,
                    };
                    let parent = self.depth.checked_sub(1).map(|index| NodeId {
                        source: self.tree.bytes,
                        offset: self.parents[index],
                    });
                    *self.parents.get_mut(self.depth).ok_or(Error::Capacity)? = offset;
                    self.depth += 1;
                    self.root_seen = true;

                    let start = self.cursor;
                    let mut count = 0;
                    while self.cursor.property(self.tree.strings)?.is_some() {
                        count += 1;
                        if count > MAX_PROPERTIES {
                            return Err(Error::Capacity);
                        }
                    }
                    let properties = Cursor {
                        bytes: &start.bytes[..self.cursor.offset - start.offset],
                        offset: start.offset,
                    };
                    return Ok(Some(Node {
                        tree: self.tree,
                        id,
                        parent,
                        name,
                        properties,
                    }));
                }
                END_NODE if self.depth != 0 => self.depth -= 1,
                END if self.root_seen && self.depth == 0 && self.cursor.bytes.is_empty() => {
                    self.ended = true;
                }
                _ => return Err(Error::Format),
            }
        }
        Ok(None)
    }
}

impl<'tree, 'a> Iterator for Nodes<'tree, 'a> {
    type Item = Node<'tree, 'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_node().expect("validated device tree structure")
    }
}

#[derive(Clone, Copy)]
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let (bytes, rest) = self.bytes.split_at_checked(length).ok_or(Error::Format)?;
        self.bytes = rest;
        self.offset += length;
        Ok(bytes)
    }

    fn word(&mut self) -> Result<u32, Error> {
        let word = self.take(4)?.first_chunk::<4>().expect("four-byte token");
        Ok(u32::from_be_bytes(*word))
    }

    fn aligned(&mut self, length: usize) -> Result<(&'a [u8], &'a [u8]), Error> {
        let padded = length.checked_next_multiple_of(4).ok_or(Error::Format)?;
        Ok(self.take(padded)?.split_at(length))
    }

    fn name(&mut self) -> Result<&'a [u8], Error> {
        let length = self
            .bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(Error::Format)?;
        let (name, padding) = self.aligned(length + 1)?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err(Error::Format);
        }
        Ok(&name[..length])
    }

    fn property(&mut self, strings: &'a [u8]) -> Result<Option<Property<'a>>, Error> {
        while !self.bytes.is_empty() {
            let pending = *self;
            match self.word()? {
                NOP => (),
                PROPERTY => {
                    let length = self.word()? as usize;
                    let offset = self.word()? as usize;
                    // QEMU firmware may leave nonzero bytes after the value.
                    // Bound the full token and keep only its declared payload.
                    let (value, _) = self.aligned(length)?;
                    let string = strings.get(offset..).ok_or(Error::Format)?;
                    let length = string
                        .iter()
                        .take(32)
                        .position(|byte| *byte == 0)
                        .filter(|length| *length != 0)
                        .ok_or(Error::Format)?;
                    let name = &string[..length];
                    if !printable(name) {
                        return Err(Error::Format);
                    }
                    return Ok(Some(Property {
                        name,
                        value,
                        offset: pending.offset,
                    }));
                }
                _ => {
                    *self = pending;
                    break;
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
#[path = "../../../tests/metal/devicetree/header.rs"]
mod tests;

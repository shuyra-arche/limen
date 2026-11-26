//! Versioned module declarations carried in a bounded ELF note.

use super::{ImageAddress, elf::Elf};
use crate::devicetree::{DeviceTree, Error as TreeError, Node, NodeId};

const NOTE_OWNER: &[u8] = b"SHUYRA\0";
const NOTE_TYPE: u32 = 1;
const NOTE_ALIGNMENT: usize = 4;
const VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptionError {
    Format,
    Capacity,
    Missing,
    Unsupported,
    Duplicate,
    Entry,
}

impl From<TreeError> for DescriptionError {
    fn from(error: TreeError) -> Self {
        match error {
            TreeError::Window | TreeError::Format | TreeError::Range => Self::Format,
            TreeError::Capacity => Self::Capacity,
            TreeError::Unsupported => Self::Unsupported,
            TreeError::Conflict => Self::Duplicate,
            TreeError::Missing => Self::Missing,
        }
    }
}

/// Exact interface declaration to match. The consumer defines what the
/// interface and calling ABI mean.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Contract<'a> {
    compatible: &'a [u8],
    abi: u32,
    version: u32,
}

impl<'a> Contract<'a> {
    pub const fn new(compatible: &'a [u8], abi: u32, version: u32) -> Self {
        Self {
            compatible,
            abi,
            version,
        }
    }

    pub const fn compatible(self) -> &'a [u8] {
        self.compatible
    }

    pub const fn abi(self) -> u32 {
        self.abi
    }

    pub const fn version(self) -> u32 {
        self.version
    }
}

/// An entry declaration tied to the original immutable note.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Interface<'a> {
    contract: Contract<'a>,
    entry: ImageAddress,
    source: NodeId<'a>,
}

impl<'a> Interface<'a> {
    pub const fn contract(self) -> Contract<'a> {
        self.contract
    }

    pub const fn version(self) -> u32 {
        self.contract.version()
    }

    /// Link-time virtual address in the file-backed executable segment.
    pub const fn entry(self) -> ImageAddress {
        self.entry
    }

    pub const fn source(self) -> NodeId<'a> {
        self.source
    }
}

/// A checked declaration from the image. Execution and platform
/// requirements are established separately when publishing an instance.
#[derive(Clone, Copy, Debug)]
pub struct Description<'a> {
    interface: Interface<'a>,
}

impl<'a> Description<'a> {
    pub(super) fn read(elf: &Elf<'a>) -> Result<Self, DescriptionError> {
        let tree = DeviceTree::new(note(elf)?)?;
        let mut nodes = tree.nodes();
        let root = nodes.next().ok_or(DescriptionError::Missing)?;
        if word(&root, b"version")? != VERSION {
            return Err(DescriptionError::Unsupported);
        }
        let abi = word(&root, b"abi")?;
        if abi == 0 {
            return Err(DescriptionError::Format);
        }

        let interfaces = nodes.next().ok_or(DescriptionError::Missing)?;
        if interfaces.parent_id() != Some(root.id()) || interfaces.name() != b"interfaces" {
            return Err(DescriptionError::Unsupported);
        }

        let declaration = nodes.next().ok_or(DescriptionError::Missing)?;
        if declaration.parent_id() != Some(interfaces.id()) {
            return Err(DescriptionError::Unsupported);
        }
        let interface = read_interface(elf, &declaration, abi)?;
        if let Some(extra) = nodes.next() {
            if extra.parent_id() == Some(interfaces.id()) {
                read_interface(elf, &extra, abi)?;
                return Err(DescriptionError::Duplicate);
            }
            return Err(DescriptionError::Unsupported);
        }
        Ok(Self { interface })
    }

    pub const fn abi(&self) -> u32 {
        self.interface.contract.abi()
    }

    pub fn interface(&self, contract: Contract<'_>) -> Option<Interface<'a>> {
        (self.interface.contract == contract).then_some(self.interface)
    }

    pub fn interfaces(&self) -> impl ExactSizeIterator<Item = Interface<'a>> {
        core::iter::once(self.interface)
    }
}

fn note<'a>(elf: &Elf<'a>) -> Result<&'a [u8], DescriptionError> {
    let mut found = None;
    for mut bytes in elf.notes() {
        // This ELF subset uses the four-byte note alignment emitted by LLD.
        while !bytes.is_empty() {
            let (header, rest) = bytes
                .split_first_chunk::<12>()
                .ok_or(DescriptionError::Format)?;
            let [name_length, descriptor_length, kind] = [
                u32::from_le_bytes(*header.first_chunk::<4>().unwrap()),
                u32::from_le_bytes(*header[4..].first_chunk::<4>().unwrap()),
                u32::from_le_bytes(*header.last_chunk::<4>().unwrap()),
            ];
            let (owner, rest) = note_field(rest, name_length)?;
            let (descriptor, rest) = note_field(rest, descriptor_length)?;
            bytes = rest;
            if owner == NOTE_OWNER && kind == NOTE_TYPE && found.replace(descriptor).is_some() {
                return Err(DescriptionError::Duplicate);
            }
        }
    }
    found.ok_or(DescriptionError::Missing)
}

fn note_field(bytes: &[u8], length: u32) -> Result<(&[u8], &[u8]), DescriptionError> {
    let length = length as usize;
    let padded = length
        .checked_next_multiple_of(NOTE_ALIGNMENT)
        .ok_or(DescriptionError::Format)?;
    let (field, rest) = bytes
        .split_at_checked(padded)
        .ok_or(DescriptionError::Format)?;
    Ok((&field[..length], rest))
}

fn read_interface<'a>(
    elf: &Elf<'a>,
    node: &Node<'_, 'a>,
    abi: u32,
) -> Result<Interface<'a>, DescriptionError> {
    let compatible = node
        .string(b"compatible")?
        .ok_or(DescriptionError::Missing)?;
    let version = word(node, b"version")?;
    if compatible.is_empty() || version == 0 {
        return Err(DescriptionError::Format);
    }
    let entry = ImageAddress::new(u64::from_be_bytes(field(node, b"entry")?));
    if !elf.is_file_executable(entry) {
        return Err(DescriptionError::Entry);
    }
    Ok(Interface {
        contract: Contract::new(compatible, abi, version),
        entry,
        source: node.id(),
    })
}

fn word(node: &Node<'_, '_>, property: &[u8]) -> Result<u32, DescriptionError> {
    field(node, property).map(u32::from_be_bytes)
}

fn field<const N: usize>(
    node: &Node<'_, '_>,
    property: &[u8],
) -> Result<[u8; N], DescriptionError> {
    node.property(property)?
        .ok_or(DescriptionError::Missing)?
        .try_into()
        .map_err(|_| DescriptionError::Format)
}

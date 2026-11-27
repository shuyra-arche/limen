//! Device tree bindings used to interpret firmware facts.

use crate::metal::devicetree::printable;
pub use crate::metal::devicetree::{DeviceTree, Error, Node, NodeId, Nodes, Property};

pub mod memory;
mod registers;

impl Node<'_, '_> {
    pub fn compatible(&self, value: &[u8]) -> Result<bool, Error> {
        let Some(bytes) = self.property(b"compatible")? else {
            return Ok(false);
        };
        let strings = bytes.strip_suffix(b"\0").ok_or(Error::Format)?;
        let mut matched = false;
        for string in strings.split(|byte| *byte == 0) {
            if string.is_empty() || !printable(string) {
                return Err(Error::Format);
            }
            matched |= string == value;
        }
        Ok(matched)
    }

    /// Availability stated by this node's own status property.
    pub fn enabled(&self) -> Result<bool, Error> {
        match self.property(b"status")? {
            None | Some(b"okay\0") => Ok(true),
            Some(b"disabled\0" | b"reserved\0" | b"fail\0") => Ok(false),
            Some(status)
                if status.strip_prefix(b"fail-").is_some_and(|reason| {
                    reason
                        .strip_suffix(b"\0")
                        .is_some_and(|reason| !reason.is_empty() && !reason.contains(&0))
                }) =>
            {
                Ok(false)
            }
            Some(_) => Err(Error::Unsupported),
        }
    }
}

pub(crate) fn cell(value: Option<&[u8]>) -> Result<Option<u32>, Error> {
    value
        .map(|bytes| {
            bytes
                .try_into()
                .map(u32::from_be_bytes)
                .map_err(|_| Error::Format)
        })
        .transpose()
}

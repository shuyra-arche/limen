//! PSCI protocol declarations and the advertised conduit.

use super::devicetree::{DeviceTree, Error, NodeId};

#[cfg(all(limen_target, target_arch = "aarch64"))]
mod native;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conduit {
    Hvc,
    Smc,
}

/// The original device-tree PSCI node and the conduit it describes.
#[derive(Clone, Copy, Debug)]
pub struct PsciDescription<'a> {
    source: NodeId<'a>,
    conduit: Conduit,
    minimum: PsciVersion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PsciVersion {
    pub major: u16,
    pub minor: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PsciError {
    Firmware(i32),
    Version,
    Affinity,
}

impl<'a> PsciDescription<'a> {
    pub fn read(tree: &DeviceTree<'a>) -> Result<Option<Self>, Error> {
        let mut description = None;
        for node in tree.nodes() {
            if !node.compatible(b"arm,psci-1.0")? && !node.compatible(b"arm,psci-0.2")? {
                continue;
            }
            if !node.enabled()? {
                continue;
            }
            let conduit = match node.property(b"method")? {
                Some(b"hvc\0") => Conduit::Hvc,
                Some(b"smc\0") => Conduit::Smc,
                None => return Err(Error::Missing),
                Some(_) => return Err(Error::Unsupported),
            };
            if description.is_some() {
                return Err(Error::Conflict);
            }
            description = Some(Self {
                source: node.id(),
                conduit,
                minimum: if node.compatible(b"arm,psci-1.0")? {
                    PsciVersion { major: 1, minor: 0 }
                } else {
                    PsciVersion { major: 0, minor: 2 }
                },
            });
        }
        Ok(description)
    }

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn conduit(self) -> Conduit {
        self.conduit
    }

    /// Minimum version declared by the original firmware binding.
    pub fn minimum_version(self) -> PsciVersion {
        self.minimum
    }
}

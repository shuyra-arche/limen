//! Describe and verify the exact PSCI conduit used by platform consumers.

#[cfg(all(limen_target, target_arch = "aarch64"))]
use super::super::psci::native::{AFFINITY_INFO, CPU_ON, FEATURES, SYSTEM_OFF, VERSION};
pub use super::super::psci::{Conduit, Psci, PsciDescription, PsciError, PsciVersion};
use crate::metal::platform::description::treaty::devicetree::{DeviceTree, Error, NodeId};

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

#[cfg(all(limen_target, target_arch = "aarch64"))]
impl PsciDescription<'_> {
    /// Query the live PSCI version and the calls required by this boot path.
    ///
    /// # Safety
    /// The caller must establish that this conduit reaches PSCI from the
    /// current exception level. A tree from an unrelated platform is invalid.
    pub unsafe fn verify(self) -> Result<Psci, PsciError> {
        let raw = unsafe { self.conduit.invoke(VERSION, 0, 0, 0) } as i32;
        if raw < 0 {
            return Err(PsciError::Firmware(raw));
        }
        let version = PsciVersion {
            major: (raw >> 16) as u16,
            minor: (raw & 0xffff) as u16,
        };
        let minimum = self.minimum_version();
        if (version.major, version.minor) < (minimum.major, minimum.minor) {
            return Err(PsciError::Version);
        }
        if version.major >= 1 {
            for function in [CPU_ON, AFFINITY_INFO, SYSTEM_OFF] {
                let raw = unsafe { self.conduit.invoke(FEATURES, function, 0, 0) } as i32;
                if raw < 0 {
                    return Err(PsciError::Firmware(raw));
                }
            }
        }
        Ok(Psci {
            conduit: self.conduit,
            version,
        })
    }
}

impl Psci {
    pub fn version(self) -> PsciVersion {
        self.version
    }

    /// Check the current PSCI state of a described affinity at level zero.
    ///
    /// # Safety
    /// The caller must have verified the conduit and the target affinity's scope.
    #[cfg(all(limen_target, target_arch = "aarch64"))]
    pub unsafe fn verify_affinity(self, affinity: usize, online: bool) -> Result<(), PsciError> {
        let raw = unsafe { self.conduit.invoke(AFFINITY_INFO, affinity, 0, 0) } as i32;
        match raw {
            0 if online => Ok(()),
            1 if !online => Ok(()),
            code if code < 0 => Err(PsciError::Firmware(code)),
            _ => Err(PsciError::Affinity),
        }
    }
}

#[cfg(all(limen_target, target_arch = "aarch64"))]
impl Conduit {
    /// Request SYSTEM_OFF through this conduit.
    ///
    /// # Safety
    /// The caller must have verified the conduit and own platform shutdown.
    pub unsafe fn system_off(self) {
        let _ = unsafe { self.invoke(SYSTEM_OFF, 0, 0, 0) };
    }
}

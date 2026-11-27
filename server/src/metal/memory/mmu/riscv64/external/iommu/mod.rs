//! RISC-V IOMMU 1.0 directory and command-queue control.

use crate::metal::{
    memory::mmu::access::device::DeviceWindow,
    platform::description::devicetree::{Error as TreeError, Node},
};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

use queue::Queue;

const CAPABILITIES: usize = 0;
const CONTROL: usize = 8;
const DDTP: usize = 0x10;
const MODE: u64 = 0xf;
const BUSY: u64 = 1 << 4;
const PPN: u64 = ((1 << 44) - 1) << 10;

pub fn registers(node: Node<'_, '_>) -> Result<Option<PhysicalRange>, TreeError> {
    if !node.enabled()? || !node.compatible(b"riscv,iommu")? {
        return Ok(None);
    }
    if node.property(b"#iommu-cells")? != Some(&1u32.to_be_bytes()) {
        return Err(TreeError::Unsupported);
    }
    Ok(Some(node.register(0)?))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Window,
    Version,
    Mode,
    Busy,
    Address,
    Rejected,
    Unconfirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Directory {
    Off,
    Bare,
    One,
    Two,
    Three,
}

impl Directory {
    fn decode(word: u64) -> Result<Self, Error> {
        match word & MODE {
            0 => Ok(Self::Off),
            1 => Ok(Self::Bare),
            2 => Ok(Self::One),
            3 => Ok(Self::Two),
            4 => Ok(Self::Three),
            _ => Err(Error::Mode),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub sv39: bool,
    pub debug_translation: bool,
    extended_context: bool,
    pub little_endian_tables: bool,
    pub address_bits: u32,
    pub directory: Directory,
}

/// Device and coherent-memory ordering supplied by the native Hart.
pub struct DeviceOrder(fn());

impl DeviceOrder {
    #[cfg(all(limen_target, target_arch = "riscv64"))]
    pub fn native() -> Self {
        Self(|| unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) })
    }

    fn fence(&self) {
        (self.0)();
    }
}

#[derive(Clone, Copy)]
struct DirectoryWord(u64);

impl DirectoryWord {
    fn mode(self) -> Directory {
        Directory::decode(self.0).expect("checked directory mode")
    }

    fn root(self) -> Option<PhysicalAddress> {
        match self.mode() {
            Directory::Off | Directory::Bare => None,
            Directory::One | Directory::Two | Directory::Three => {
                Some(PhysicalAddress::new((self.0 & PPN) << 2))
            }
        }
    }

    fn off(self) -> Self {
        // The PPN must remain unchanged on entry to Off. Preserve WPRI fields.
        Self(self.0 & !MODE)
    }

    fn one(self, root: PhysicalAddress, address_bits: u32) -> Result<Self, Error> {
        let address = root.get();
        if !address.is_multiple_of(4096) || address >> address_bits.min(56) != 0 {
            return Err(Error::Address);
        }
        Ok(Self((self.0 & !(PPN | MODE)) | (address >> 2) | 2))
    }

    fn confirms(self, target: Self) -> bool {
        self.mode() == target.mode()
            && (target.mode() == Directory::Off || self.0 & PPN == target.0 & PPN)
    }
}

enum Selection {
    Selected(DirectoryWord),
    Pending {
        original: DirectoryWord,
        target: DirectoryWord,
    },
    Unconfirmed {
        original: DirectoryWord,
        target: DirectoryWord,
    },
}

/// Exclusive authority over a physical register file. Retains the installed
/// directory, all unsettled directory writes and the optional command queue.
#[must_use]
pub struct Controller<'a> {
    window: &'a mut DeviceWindow,
    order: DeviceOrder,
    profile: Profile,
    directory: Selection,
    retained_root: Option<PhysicalAddress>,
    queue: Option<Queue>,
}

impl<'a> Controller<'a> {
    /// # Safety
    /// Map the actual little-endian IOMMU register file, exclusively control
    /// the device and its requesters, and retain translation and queue resources
    /// through all their hardware obligations, including loss of this owner.
    /// All device-visible RAM used through this controller must be coherent.
    pub unsafe fn admit(window: &'a mut DeviceWindow, order: DeviceOrder) -> Result<Self, Error> {
        let caps =
            u64::from_le(unsafe { window.read_u64(CAPABILITIES) }.map_err(|_| Error::Window)?);
        if caps as u8 != 0x10 {
            return Err(Error::Version);
        }
        let control = u32::from_le(unsafe { window.read_u32(CONTROL) }.map_err(|_| Error::Window)?);
        let directory = unsafe { read_directory(window)? };
        let profile = Profile {
            extended_context: caps & (1 << 22) != 0,
            debug_translation: caps & (1 << 31) != 0,
            little_endian_tables: control & 1 == 0,
            address_bits: ((caps >> 32) & 0x3f) as u32,
            sv39: caps & (1 << 9) != 0,
            directory: directory.mode(),
        };
        Ok(Self {
            window,
            order,
            profile,
            directory: Selection::Selected(directory),
            retained_root: None,
            queue: None,
        })
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The retired directory still needs its independent settlement proof.
    pub fn retained_root(&self) -> Option<PhysicalAddress> {
        self.retained_root
    }

    /// Directory roots whose selection cannot be established.
    pub fn uncertain_roots(&self) -> Option<[Option<PhysicalAddress>; 2]> {
        match self.directory {
            Selection::Unconfirmed { original, target } => Some([original.root(), target.root()]),
            _ => None,
        }
    }

    fn selected(&self) -> Result<DirectoryWord, Error> {
        match self.directory {
            Selection::Selected(word) => Ok(word),
            Selection::Pending { .. } => Err(Error::Busy),
            Selection::Unconfirmed { .. } => Err(Error::Unconfirmed),
        }
    }

    fn issue_directory(
        &mut self,
        original: DirectoryWord,
        target: DirectoryWord,
    ) -> Result<(), Error> {
        if self.queue.is_some() {
            return Err(Error::Mode);
        }
        self.directory = Selection::Pending { original, target };
        self.order.fence();
        unsafe { self.window.write_u64(DDTP, target.0.to_le()) }.map_err(|_| Error::Window)
    }

    /// # Safety
    /// Quiesce all affected requesters. Turning translation off does not retire
    /// cached directory entries or complete existing device transactions.
    pub unsafe fn begin_shut(&mut self) -> Result<(), Error> {
        let original = self.selected()?;
        self.issue_directory(original, original.off())
    }

    /// # Safety
    /// The root must name a fully initialized, pinned and coherent 4 KiB
    /// directory. Retain it and all reachable tables through their hardware
    /// obligations. Keep requesters isolated until old translations and
    /// transactions are independently settled.
    pub unsafe fn begin_install(&mut self, root: PhysicalAddress) -> Result<(), Error> {
        let original = self.selected()?;
        if original.mode() != Directory::Off || self.retained_root.is_some() {
            return Err(Error::Mode);
        }
        let target = original.one(root, self.profile.address_bits)?;
        self.issue_directory(original, target)
    }

    /// Observe only the original register write. Delayed readback never
    /// resubmits it, and a contradictory readback blocks further programming.
    pub fn observe_directory(&mut self) -> Result<Directory, Error> {
        let Selection::Pending { original, target } = self.directory else {
            self.selected()?;
            return Err(Error::Mode);
        };
        self.order.fence();
        let actual = match unsafe { read_directory(self.window) } {
            Err(Error::Mode) => {
                self.directory = Selection::Unconfirmed { original, target };
                return Err(Error::Unconfirmed);
            }
            result => result?,
        };
        if !actual.confirms(target) {
            self.directory = Selection::Unconfirmed { original, target };
            return Err(Error::Unconfirmed);
        }
        if actual.mode() == Directory::Off
            && let Some(root) = original.root()
        {
            self.retained_root = Some(root);
        }
        self.profile.directory = actual.mode();
        self.directory = Selection::Selected(actual);
        Ok(actual.mode())
    }

    /// # Safety
    /// Access only the hardware debug-translation register block. Debug
    /// requests must not outlive this borrow.
    pub unsafe fn debug_registers(&mut self) -> Result<&mut DeviceWindow, Error> {
        self.selected()?;
        if !self.profile.debug_translation || self.queue.is_some() {
            return Err(Error::Mode);
        }
        Ok(self.window)
    }

    /// # Safety
    /// The page must be exclusively owned, at exactly `physical` in pinned,
    /// coherent memory, and retained through shutdown, faults, or loss of this
    /// controller. No independent producer may access the queue.
    pub unsafe fn open_queue(
        &mut self,
        page: &'static mut queue::Page,
        physical: PhysicalAddress,
    ) -> Result<(), Error> {
        self.selected()?;
        if self.queue.is_some() || !self.profile.little_endian_tables {
            return Err(Error::Mode);
        }
        if physical.get() >> self.profile.address_bits.min(56) != 0 {
            return Err(Error::Address);
        }
        let queue = unsafe { Queue::prepare(self.window, page, physical) }?;
        self.queue.insert(queue).start(self.window, &self.order)
    }

    /// The original queue storage, including an unconfirmed installation.
    pub fn retained_queue(&self) -> Option<PhysicalAddress> {
        self.queue.as_ref().map(Queue::physical)
    }

    pub fn submit(&mut self, operation: queue::Invalidate) -> Result<(), Error> {
        self.queue
            .as_mut()
            .ok_or(Error::Mode)?
            .submit(self.window, &self.order, operation)
    }

    pub fn observe(&mut self) -> Result<queue::Invalidate, Error> {
        self.queue
            .as_mut()
            .ok_or(Error::Mode)?
            .observe(self.window, &self.order)
    }

    /// The exact outstanding operation, including after queue faults.
    pub fn unsettled_invalidation(&self) -> Option<queue::Invalidate> {
        self.queue.as_ref().and_then(Queue::unsettled)
    }

    pub fn shut_queue(&mut self) -> Result<(), Error> {
        self.queue
            .as_mut()
            .ok_or(Error::Mode)?
            .shut(self.window, &self.order)?;
        self.queue = None;
        Ok(())
    }
}

unsafe fn read_directory(window: &mut DeviceWindow) -> Result<DirectoryWord, Error> {
    let word = u64::from_le(unsafe { window.read_u64(DDTP) }.map_err(|_| Error::Window)?);
    if word & BUSY != 0 {
        return Err(Error::Busy);
    }
    Directory::decode(word)?;
    Ok(DirectoryWord(word))
}

pub mod queue;
pub mod translation;

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/riscv64/external/iommu/mod.rs"]
mod tests;

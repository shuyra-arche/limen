//! AMD-Vi unsegmented device table and command queue on a coherent x86 host.

pub mod binding;
pub mod directory;
pub mod queue;

use crate::metal::{
    memory::access::treaty::device::DeviceWindow,
    platform::description::acpi::{DeviceSelections, Ivhd},
};
use binding::{Admissions, Binding};
use core::sync::atomic::{Ordering, fence};
use directory::Table;
use limen_treaty::model::PhysicalAddress;
use queue::{Completion, Progress, Queue};

const TABLE: usize = 0;
const COMMANDS: usize = 8;
const CONTROL: usize = 0x18;
const EXCLUSION: usize = 0x20;
const FEATURES: usize = 0x30;
const HEAD: usize = 0x2000;
const TAIL: usize = 0x2008;
const STATUS: usize = 0x2020;
const ENABLE: u64 = 1 | (1 << 12);
const RUNNING: u64 = 1 << 4;
/// The published physical mappings of these exact objects remain with the
/// caller, including after timeout, drop, or terminal test shutdown.
pub struct Storage<'a> {
    pub table: (&'a mut Table, PhysicalAddress),
    pub queue: (&'a mut Queue, PhysicalAddress),
    pub completion: (&'a mut Completion, PhysicalAddress),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Window,
    Unsupported,
    Occupied,
    Address,
    Pending,
    Unconfirmed,
    Exhausted,
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Fresh,
    Starting,
    Waiting(u64),
    Ready,
    Unconfirmed,
}

#[must_use]
pub struct Controller<'a> {
    unit: Ivhd<'a>,
    selections: DeviceSelections<'a>,
    window: DeviceWindow<'a>,
    storage: Storage<'a>,
    progress: Progress,
    bindings: &'a mut [Option<Binding>],
    admissions: &'a mut Admissions,
    next: u64,
    tail: usize,
}

impl<'a> Controller<'a> {
    /// # Safety
    /// This is the described unit's x86 UC register window. Its actual PCI
    /// capability and requester coverage have been checked. The caller owns
    /// the reset-state unit and all sources exclusively, with no pending DMA,
    /// ATS, cached endpoint translations or bypass. Each physical address
    /// names its object in coherent, contiguous, pinned RAM. The table is
    /// fresh Table::DENIED and all storage remains reserved while hardware can
    /// reference it, including if this owner is dropped. Re-admission after
    /// external reset requires settling every old device effect. Supply
    /// this exact unit's original resident admission history even if all
    /// previous controllers and their storage have been replaced.
    pub unsafe fn admit(
        unit: Ivhd<'a>,
        window: &'a mut DeviceWindow<'_>,
        storage: Storage<'a>,
        bindings: &'a mut [Option<Binding>],
        admissions: &'a mut Admissions,
    ) -> Result<Self, Error> {
        if bindings.is_empty()
            || bindings.len() > usize::from(u16::MAX) + 1
            || bindings.iter().any(Option::is_some)
        {
            return Err(Error::Occupied);
        }
        if window.physical() != unit.registers() || !window.supports_u64(STATUS) {
            return Err(Error::Window);
        }
        let table = storage.table.1.get();
        let queue = storage.queue.1.get();
        let completion = storage.completion.1.get();
        let ranges = [
            (table, size_of::<Table>() as u64, 4096),
            (queue, size_of::<Queue>() as u64, 4096),
            (completion, 8, 8),
        ];
        for (i, &(base, size, align)) in ranges.iter().enumerate() {
            if base == 0
                || !base.is_multiple_of(align)
                || base > (1 << 52) - size
                || ranges[..i]
                    .iter()
                    .any(|&(other, len, _)| base < other + len && other < base + size)
            {
                return Err(Error::Address);
            }
        }
        let mut controller = Self {
            unit,
            selections: unit.selections(),
            window: window.reborrow(),
            storage,
            progress: Progress::Fresh,
            bindings,
            admissions,
            next: 0,
            tail: 0,
        };
        if controller.read(FEATURES) & (1 << 6) == 0 {
            return Err(Error::Unsupported);
        }
        if controller.read(CONTROL) != 0
            || controller.read(EXCLUSION) & 1 != 0
            || controller.read(STATUS) != 0
        {
            return Err(Error::Occupied);
        }
        controller.admissions.advance(unit.into())?;
        Ok(controller)
    }

    fn read(&mut self, register: usize) -> u64 {
        unsafe { self.window.read_u64(register) }.expect("checked AMD-Vi register")
    }
    fn write(&mut self, register: usize, value: u64) {
        unsafe { self.window.write_u64(register, value) }.expect("checked AMD-Vi register");
    }

    /// Enable the resident, initially denying table. The caller can continue a
    /// pending transition by calling poll without reissuing any command.
    pub fn install_blocked(&mut self) -> Result<(), Error> {
        if !matches!(self.progress, Progress::Fresh) {
            return Err(Error::Occupied);
        }
        self.progress = Progress::Starting;
        let table = self.storage.table.1.get() | 511;
        let queue = self.storage.queue.1.get() | (8 << 56);
        fence(Ordering::SeqCst);
        self.write(TABLE, table);
        self.write(COMMANDS, queue);
        self.write(HEAD, 0);
        self.write(TAIL, 0);
        if self.read(TABLE) != table
            || self.read(COMMANDS) != queue
            || self.read(HEAD) != 0
            || self.read(TAIL) != 0
        {
            return Err(self.uncertain());
        }
        self.write(CONTROL, ENABLE);
        self.poll()
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/x86_64/external/amdvi/mod.rs"]
mod tests;

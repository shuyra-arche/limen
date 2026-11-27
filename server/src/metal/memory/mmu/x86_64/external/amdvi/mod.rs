//! AMD-Vi unsegmented device table and command queue on a coherent x86 host.

use crate::metal::{
    memory::mmu::access::device::DeviceWindow,
    platform::description::acpi::{DeviceSelections, Ivhd},
};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, Ordering, fence},
};
use limen_treaty::model::PhysicalAddress;

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
const ADDRESS: u64 = 0x000f_ffff_ffff_f000;
// V and TV are set with a nonzero paging mode, but the DTE grants
// neither read nor write, so no ordinary device access may proceed.
const DENY: [u64; 4] = [3 | (1 << 9), 0, 0, 0];

// A physical reset can re-admit the same MMIO address and restart command
// numbers. Its published requester identities must use a fresh epoch.
static ADMISSIONS: AtomicU64 = AtomicU64::new(1);

fn reserve_epoch(epochs: &AtomicU64) -> Result<u64, Error> {
    let previous = epochs.load(Ordering::Relaxed);
    let next = previous.checked_add(1).ok_or(Error::Exhausted)?;
    epochs
        .compare_exchange(previous, next, Ordering::Relaxed, Ordering::Relaxed)
        .map_err(|_| Error::Occupied)
}

/// Resident, naturally aligned device table covering every 16-bit DeviceID.
#[repr(C, align(4096))]
pub struct Table([[u64; 4]; 65536]);

impl Table {
    pub const DENIED: Self = Self([DENY; 65536]);
}

#[repr(C, align(4096))]
pub struct Queue([[u64; 2]; 256]);

impl Queue {
    pub const EMPTY: Self = Self([[0; 2]; 256]);
}

#[repr(C, align(8))]
pub struct Completion(UnsafeCell<u64>);

impl Completion {
    pub const fn zeroed() -> Self {
        Self(UnsafeCell::new(0))
    }
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Context {
    pub requester: u16,
    pub domain: u16,
    pub root: PhysicalAddress,
    pub levels: u8,
}

/// The exact admitted generation of a requester on one physical unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    unit: PhysicalAddress,
    epoch: u64,
    number: u64,
    context: Context,
}

impl Binding {
    pub fn context(self) -> Context {
        self.context
    }
}

#[derive(Clone, Copy)]
enum Operation {
    InstallRequester(u16),
    InstallAll,
    Bind(usize),
    Unbind(usize),
}

#[derive(Clone, Copy)]
struct Batch {
    number: u64,
    operation: Operation,
}

enum Progress {
    Fresh,
    Starting,
    Waiting(Batch),
    Ready,
    Unconfirmed(Option<Batch>),
}

#[must_use]
pub struct Controller<'a> {
    unit: Ivhd<'a>,
    selections: DeviceSelections<'a>,
    window: &'a mut DeviceWindow,
    storage: Storage<'a>,
    progress: Progress,
    bindings: &'a mut [Option<Binding>],
    epoch: u64,
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
    /// external reset requires settling every old device effect.
    pub unsafe fn admit(
        unit: Ivhd<'a>,
        window: &'a mut DeviceWindow,
        storage: Storage<'a>,
        bindings: &'a mut [Option<Binding>],
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
            window,
            storage,
            progress: Progress::Fresh,
            bindings,
            epoch: 0,
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
        controller.epoch = reserve_epoch(&ADMISSIONS)?;
        Ok(controller)
    }

    fn read(&mut self, register: usize) -> u64 {
        unsafe { self.window.read_u64(register) }.expect("checked AMD-Vi register")
    }
    fn write(&mut self, register: usize, value: u64) {
        unsafe { self.window.write_u64(register, value) }.expect("checked AMD-Vi register");
    }

    pub fn state(&self) -> State {
        match self.progress {
            Progress::Fresh => State::Fresh,
            Progress::Starting => State::Starting,
            Progress::Waiting(batch) => State::Waiting(batch.number),
            Progress::Ready => State::Ready,
            Progress::Unconfirmed(_) => State::Unconfirmed,
        }
    }

    /// The binding is usable only after the original batch settles.
    pub fn binding(&self, requester: u16) -> Option<Binding> {
        let (index, binding) = self.find(requester)?;
        match self.progress {
            Progress::Ready => Some(binding),
            Progress::Waiting(Batch {
                operation: Operation::Bind(pending) | Operation::Unbind(pending),
                ..
            }) if pending != index => Some(binding),
            _ => None,
        }
    }

    /// When completion is uncertain, keep the exact operation and both
    /// potentially observed translations. In particular, an unbind leaves
    /// its original binding reserved until an independent retirement.
    pub fn unsettled_generation(&self) -> Option<u64> {
        match self.progress {
            Progress::Waiting(batch) | Progress::Unconfirmed(Some(batch)) => Some(batch.number),
            _ => None,
        }
    }

    fn find(&self, requester: u16) -> Option<(usize, Binding)> {
        self.bindings.iter().enumerate().find_map(|(index, entry)| {
            entry
                .filter(|entry| entry.context.requester == requester)
                .map(|entry| (index, entry))
        })
    }

    fn uncertain(&mut self) -> Error {
        let pending = match self.progress {
            Progress::Waiting(batch) => Some(batch),
            _ => None,
        };
        self.progress = Progress::Unconfirmed(pending);
        Error::Unconfirmed
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

    fn next_install_batch(&mut self) {
        let operation = self
            .selections
            .next()
            .map_or(Operation::InstallAll, Operation::InstallRequester);
        self.invalidate(operation);
    }

    fn invalidate(&mut self, operation: Operation) {
        // At most three slots are used, and the previous completion store
        // has already settled. No independent producer advances this tail.
        self.next += 1;
        let commands = &mut self.storage.queue.0.0;
        let requester = match operation {
            Operation::InstallRequester(requester) => Some(requester),
            Operation::Bind(index) | Operation::Unbind(index) => Some(
                self.bindings[index]
                    .expect("reserved binding")
                    .context
                    .requester,
            ),
            Operation::InstallAll => None,
        };
        if let Some(requester) = requester {
            commands[self.tail] = [(2 << 60) | u64::from(requester), 0];
            self.tail = (self.tail + 1) % commands.len();
        }
        if !matches!(operation, Operation::InstallRequester(_)) {
            commands[self.tail] = [8 << 60, 0]; // INVALIDATE_ALL, only with IASup.
            self.tail = (self.tail + 1) % commands.len();
        }
        // S=1, F=1: ordered completion store after invalidation.
        commands[self.tail] = [(1 << 60) | self.storage.completion.1.get() | 5, self.next];
        self.tail = (self.tail + 1) % commands.len();
        unsafe { self.storage.completion.0.0.get().write_volatile(0) };
        self.progress = Progress::Waiting(Batch {
            number: self.next,
            operation,
        });
        fence(Ordering::SeqCst);
        self.write(TAIL, (self.tail * 16) as u64);
    }

    pub fn poll(&mut self) -> Result<(), Error> {
        for _ in 0..256 {
            match self.progress {
                Progress::Starting => {
                    if self.read(CONTROL) != ENABLE {
                        return Err(self.uncertain());
                    }
                    if self.read(STATUS) & RUNNING != 0 {
                        self.next_install_batch();
                    }
                }
                Progress::Waiting(batch) => {
                    let completion = unsafe { self.storage.completion.0.0.get().read_volatile() };
                    if self.read(STATUS) & RUNNING == 0 {
                        return Err(self.uncertain());
                    }
                    if completion == batch.number {
                        fence(Ordering::Acquire);
                        match batch.operation {
                            Operation::InstallRequester(_) => {
                                self.next_install_batch();
                                continue;
                            }
                            Operation::Unbind(index) => self.bindings[index] = None,
                            Operation::Bind(_) | Operation::InstallAll => {}
                        }
                        self.progress = Progress::Ready;
                        return Ok(());
                    }
                }
                Progress::Ready => return Ok(()),
                Progress::Unconfirmed(_) => return Err(Error::Unconfirmed),
                Progress::Fresh => return Err(Error::Occupied),
            }
            core::hint::spin_loop();
        }
        Err(Error::Pending)
    }

    /// # Safety
    /// The selected requester and outstanding fabric requests are quiesced
    /// through completion. It has no aliases, ATS or interrupt remapping
    /// obligations. The complete host table remains pinned and visible
    /// through unbind and matching completion. The caller controls its
    /// nonzero domain exclusively within the physical IOMMU.
    pub unsafe fn bind(&mut self, context: Context) -> Result<(), Error> {
        if !matches!(self.progress, Progress::Ready) {
            return Err(Error::Occupied);
        }
        if !self.unit.selects(context.requester) {
            return Err(Error::Unsupported);
        }
        if context.domain == 0
            || !(1..=3).contains(&context.levels)
            || context.root.get() == 0
            || context.root.get() & !ADDRESS != 0
        {
            return Err(Error::Address);
        }
        let number = self.next.checked_add(1).ok_or(Error::Exhausted)?;
        let index = admit_binding(self.bindings, context)?;
        self.bindings[index] = Some(Binding {
            unit: self.unit.registers().start(),
            epoch: self.epoch,
            number,
            context,
        });
        let dte = &mut self.storage.table.0.0[usize::from(context.requester)];
        dte[1] = u64::from(context.domain);
        dte[0] = context.root.get() | (u64::from(context.levels) << 9) | 3 | (3 << 61);
        self.invalidate(Operation::Bind(index));
        self.poll()
    }

    /// # Safety
    /// Stop the original requester and drain all old device and fabric
    /// requests before unbinding. Retain its tables until the command and
    /// matching completion store settle. Endpoint ATS is outside this profile.
    pub unsafe fn unbind(&mut self, expected: Binding) -> Result<(), Error> {
        if !matches!(self.progress, Progress::Ready) {
            return Err(Error::Occupied);
        }
        let index = binding_slot(self.bindings, expected)?;
        if self.next == u64::MAX {
            return Err(Error::Exhausted);
        }
        self.storage.table.0.0[usize::from(expected.context.requester)] = DENY;
        self.invalidate(Operation::Unbind(index));
        self.poll()
    }
}

fn binding_slot(bindings: &[Option<Binding>], expected: Binding) -> Result<usize, Error> {
    bindings
        .iter()
        .position(|entry| *entry == Some(expected))
        .ok_or(Error::Stale)
}

fn admit_binding(bindings: &[Option<Binding>], context: Context) -> Result<usize, Error> {
    let mut vacant = None;
    for (index, entry) in bindings.iter().enumerate() {
        match entry {
            Some(entry)
                if entry.context.requester == context.requester
                    || entry.context.domain == context.domain =>
            {
                return Err(Error::Occupied);
            }
            None if vacant.is_none() => vacant = Some(index),
            _ => {}
        }
    }
    vacant.ok_or(Error::Exhausted)
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/x86_64/external/amdvi/mod.rs"]
mod tests;

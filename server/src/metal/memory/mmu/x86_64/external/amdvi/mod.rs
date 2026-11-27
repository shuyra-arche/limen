//! AMD-Vi unsegmented device table and command queue on a coherent x86 host.

use crate::metal::{
    memory::mmu::{Retirement, Unbind, access::device::DeviceWindow, retirement::Sealed},
    platform::description::acpi::{DeviceSelections, Ivhd},
};
use core::{
    cell::UnsafeCell,
    sync::atomic::{Ordering, fence},
};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

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

/// Resident admission history for one described physical unit. Its owner
/// keeps this record through restarts and replacement of the controller.
pub struct Admissions {
    scope: Scope,
    epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Scope {
    registers: PhysicalRange,
    segment: u16,
    device: u16,
}

impl From<Ivhd<'_>> for Scope {
    fn from(unit: Ivhd<'_>) -> Self {
        Self {
            registers: unit.registers(),
            segment: unit.segment(),
            device: unit.device(),
        }
    }
}

impl Admissions {
    pub fn new(unit: Ivhd<'_>) -> Self {
        Self {
            scope: unit.into(),
            epoch: 0,
        }
    }

    fn advance(&mut self, scope: Scope) -> Result<(), Error> {
        if self.scope != scope {
            return Err(Error::Window);
        }
        self.epoch = self.epoch.checked_add(1).ok_or(Error::Exhausted)?;
        Ok(())
    }
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
    scope: Scope,
    epoch: u64,
    number: u64,
    retirement: u64,
    context: Context,
}

impl Binding {
    pub fn context(self) -> Context {
        self.context
    }
}

/// Observation of the completion store reserved by this original binding.
pub struct Unbinding {
    binding: Binding,
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

    fn next_install_batch(&mut self) -> Result<(), Error> {
        let operation = self
            .selections
            .next()
            .map_or(Operation::InstallAll, Operation::InstallRequester);
        let number = self.next.checked_add(1).ok_or_else(|| self.uncertain())?;
        self.next = number;
        self.invalidate(number, operation);
        Ok(())
    }

    fn invalidate(&mut self, number: u64, operation: Operation) {
        self.progress = Progress::Waiting(Batch { number, operation });
        // At most three slots are used, and the previous completion store
        // has already settled. No independent producer advances this tail.
        let commands = &mut self.storage.queue.0.0;
        let requester = match operation {
            Operation::InstallRequester(requester) => Some(requester),
            Operation::Bind(index) | Operation::Unbind(index) => {
                let requester = self.bindings[index]
                    .expect("reserved binding")
                    .context
                    .requester;
                if matches!(operation, Operation::Unbind(_)) {
                    self.storage.table.0.0[usize::from(requester)] = DENY;
                }
                Some(requester)
            }
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
        commands[self.tail] = [(1 << 60) | self.storage.completion.1.get() | 5, number];
        self.tail = (self.tail + 1) % commands.len();
        unsafe { self.storage.completion.0.0.get().write_volatile(0) };
        fence(Ordering::SeqCst);
        self.write(TAIL, (self.tail * 16) as u64);
    }

    /// Continue an original table installation or binding publication.
    pub fn poll(&mut self) -> Result<(), Error> {
        for _ in 0..256 {
            match self.progress {
                Progress::Starting => {
                    if self.read(CONTROL) != ENABLE {
                        return Err(self.uncertain());
                    }
                    if self.read(STATUS) & RUNNING != 0 {
                        self.next_install_batch()?;
                    }
                }
                Progress::Waiting(batch) => {
                    if matches!(batch.operation, Operation::Unbind(_)) {
                        return Err(Error::Occupied);
                    }
                    match self.observe_batch(batch) {
                        Ok(()) if matches!(batch.operation, Operation::InstallRequester(_)) => {
                            self.next_install_batch()?;
                            continue;
                        }
                        Ok(()) => {
                            self.progress = Progress::Ready;
                            return Ok(());
                        }
                        Err(Error::Pending) => {}
                        Err(error) => return Err(error),
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

    fn observe_batch(&mut self, batch: Batch) -> Result<(), Error> {
        let completion = unsafe { self.storage.completion.0.0.get().read_volatile() };
        if self.read(STATUS) & RUNNING == 0 {
            return Err(self.uncertain());
        }
        if completion != batch.number {
            return Err(Error::Pending);
        }
        fence(Ordering::Acquire);
        Ok(())
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
        let index = admit_binding(self.bindings, context)?;
        let (number, retirement) = reserve_binding_numbers(&mut self.next)?;
        self.bindings[index] = Some(Binding {
            scope: self.admissions.scope,
            epoch: self.admissions.epoch,
            number,
            retirement,
            context,
        });
        let dte = &mut self.storage.table.0.0[usize::from(context.requester)];
        dte[1] = u64::from(context.domain);
        dte[0] = context.root.get() | (u64::from(context.levels) << 9) | 3 | (3 << 61);
        self.invalidate(number, Operation::Bind(index));
        self.poll()
    }

    fn retirement_batch(&self, binding: Binding) -> Option<(Batch, usize)> {
        let batch = match self.progress {
            Progress::Waiting(batch) | Progress::Unconfirmed(Some(batch)) => batch,
            _ => return None,
        };
        let Operation::Unbind(index) = batch.operation else {
            return None;
        };
        (batch.number == binding.retirement && self.bindings[index] == Some(binding))
            .then_some((batch, index))
    }
}

impl Sealed for Controller<'_> {}

impl Unbind for Controller<'_> {
    type Binding = Binding;
    type Ticket = Unbinding;
    type Error = Error;

    unsafe fn retire(&mut self, binding: Binding) -> Result<Unbinding, Error> {
        if !matches!(self.progress, Progress::Ready) {
            return Err(Error::Occupied);
        }
        let index = binding_slot(self.bindings, binding)?;
        self.invalidate(binding.retirement, Operation::Unbind(index));
        Ok(Unbinding { binding })
    }

    fn pending_retirement(&self, binding: Binding) -> Option<Unbinding> {
        self.retirement_batch(binding)
            .map(|_| Unbinding { binding })
    }

    fn observe_retirement(&mut self, ticket: &Unbinding) -> Result<Retirement, Error> {
        let (batch, index) = self.retirement_batch(ticket.binding).ok_or(Error::Stale)?;
        if matches!(self.progress, Progress::Unconfirmed(_)) {
            return Err(Error::Unconfirmed);
        }
        match self.observe_batch(batch) {
            Ok(()) => {
                self.bindings[index] = None;
                self.progress = Progress::Ready;
                Ok(Retirement::Settled)
            }
            Err(Error::Pending) => Ok(Retirement::Pending),
            Err(error) => Err(error),
        }
    }
}

fn reserve_binding_numbers(next: &mut u64) -> Result<(u64, u64), Error> {
    // Allocate the exact completion stamps for both transitions before any
    // table write. A later admission cannot exhaust this binding's teardown.
    let retirement = next.checked_add(2).ok_or(Error::Exhausted)?;
    *next = retirement;
    Ok((retirement - 1, retirement))
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

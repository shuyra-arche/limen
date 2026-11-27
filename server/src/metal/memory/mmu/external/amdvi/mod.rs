//! AMD-Vi unsegmented device table and command queue on a coherent x86 host.

use crate::metal::{
    memory::mmu::internal::device::DeviceWindow, platform::description::acpi::Ivhd,
};
use core::{
    cell::UnsafeCell,
    sync::atomic::{Ordering, fence},
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
    pub const EMPTY: Self = Self(UnsafeCell::new(0));
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

#[must_use]
pub struct Controller<'a> {
    requester: u16,
    window: &'a mut DeviceWindow,
    storage: Storage<'a>,
    state: State,
    context: Option<Context>,
    retiring: bool,
    next: u64,
    tail: usize,
}

impl<'a> Controller<'a> {
    /// # Safety
    /// This is the described unit's x86 UC register window. Its actual PCI
    /// capability and requester coverage have been checked. The caller owns
    /// the reset-state unit and all sources exclusively, with no pending DMA,
    /// ATS, cached endpoint translations or bypass. Other requesters stay
    /// quiesced for the lifetime of this single-requester profile. Each address names its
    /// accompanying object in coherent, contiguous, permanently pinned RAM.
    /// The table is fresh Table::DENIED and all objects remain reserved while
    /// hardware can reference them, including if this owner is dropped.
    pub unsafe fn admit(
        unit: Ivhd<'_>,
        requester: u16,
        window: &'a mut DeviceWindow,
        storage: Storage<'a>,
    ) -> Result<Self, Error> {
        if !unit.selects(requester) {
            return Err(Error::Unsupported);
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
            requester,
            window,
            storage,
            state: State::Fresh,
            context: None,
            retiring: false,
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
        Ok(controller)
    }

    fn read(&mut self, register: usize) -> u64 {
        unsafe { self.window.read_u64(register) }.expect("checked AMD-Vi register")
    }
    fn write(&mut self, register: usize, value: u64) {
        unsafe { self.window.write_u64(register, value) }.expect("checked AMD-Vi register");
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn context(&self) -> Option<Context> {
        self.context
    }

    fn uncertain(&mut self) -> Error {
        self.state = State::Unconfirmed;
        Error::Unconfirmed
    }

    /// Enable the resident, initially denying table. The caller can continue a
    /// pending transition by calling poll without reissuing any command.
    pub fn install_blocked(&mut self) -> Result<(), Error> {
        if self.state != State::Fresh {
            return Err(Error::Occupied);
        }
        self.state = State::Starting;
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

    fn invalidate(&mut self) {
        // Three slots are always available: a new batch starts only after the
        // previous completion store. No other producer advances this tail.
        self.next += 1;
        let commands = &mut self.storage.queue.0.0;
        commands[self.tail] = [(2 << 60) | u64::from(self.requester), 0];
        self.tail = (self.tail + 1) % commands.len();
        commands[self.tail] = [8 << 60, 0]; // INVALIDATE_ALL, only with IASup.
        self.tail = (self.tail + 1) % commands.len();
        // S=1, F=1: ordered completion store after invalidation.
        commands[self.tail] = [(1 << 60) | self.storage.completion.1.get() | 5, self.next];
        self.tail = (self.tail + 1) % commands.len();
        unsafe { self.storage.completion.0.0.get().write_volatile(0) };
        self.state = State::Waiting(self.next);
        fence(Ordering::SeqCst);
        self.write(TAIL, (self.tail * 16) as u64);
    }

    pub fn poll(&mut self) -> Result<(), Error> {
        for _ in 0..256 {
            match self.state {
                State::Starting => {
                    if self.read(CONTROL) != ENABLE {
                        return Err(self.uncertain());
                    }
                    if self.read(STATUS) & RUNNING != 0 {
                        self.invalidate()
                    }
                }
                State::Waiting(expected) => {
                    let completion = unsafe { self.storage.completion.0.0.get().read_volatile() };
                    if self.read(STATUS) & RUNNING == 0 {
                        return Err(self.uncertain());
                    }
                    if completion == expected {
                        fence(Ordering::Acquire);
                        if self.retiring {
                            self.context = None;
                            self.retiring = false;
                        }
                        self.state = State::Ready;
                        return Ok(());
                    }
                }
                State::Ready => return Ok(()),
                State::Unconfirmed => return Err(Error::Unconfirmed),
                State::Fresh => return Err(Error::Occupied),
            }
            core::hint::spin_loop();
        }
        Err(Error::Pending)
    }

    /// # Safety
    /// All covered requesters and outstanding fabric requests are quiesced and
    /// will remain so until completion. The selected requester has no aliases,
    /// ATS or interrupt remapping obligations. The complete valid host page
    /// table with its grants remains pinned and visible through unbind and its
    /// matching completion. The caller owns the nonzero domain exclusively.
    pub unsafe fn bind(&mut self, context: Context) -> Result<(), Error> {
        if self.state != State::Ready || self.context.is_some() {
            return Err(Error::Occupied);
        }
        if self.requester != context.requester
            || context.domain == 0
            || !(1..=3).contains(&context.levels)
            || context.root.get() == 0
            || context.root.get() & !ADDRESS != 0
        {
            return Err(Error::Address);
        }
        if self.next == u64::MAX {
            return Err(Error::Exhausted);
        }
        let dte = &mut self.storage.table.0.0[usize::from(context.requester)];
        dte[1] = u64::from(context.domain);
        dte[0] = context.root.get() | (u64::from(context.levels) << 9) | 3 | (3 << 61);
        self.context = Some(context);
        self.invalidate();
        self.poll()
    }

    /// # Safety
    /// Stop the original requester and drain all old business and fabric
    /// requests before unbinding. Retain its tables until the command and
    /// matching completion store settle. Endpoint ATS is outside this profile.
    pub unsafe fn unbind(&mut self, expected: Context) -> Result<(), Error> {
        if self.state != State::Ready || self.context != Some(expected) {
            return Err(Error::Occupied);
        }
        if self.next == u64::MAX {
            return Err(Error::Exhausted);
        }
        self.retiring = true;
        self.storage.table.0.0[usize::from(expected.requester)] = DENY;
        self.invalidate();
        self.poll()
    }
}

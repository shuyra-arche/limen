//! RV64 cooperative continuation transfer within the supervisor address space.

use core::{
    arch::{asm, global_asm},
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::offset_of,
};

use super::boot;
use crate::basis::room::Occupancy;

#[repr(C)]
struct Registers {
    resume: usize,
    stack: usize,
    preserved: [usize; 12],
}

impl Registers {
    const EMPTY: Self = Self {
        resume: 0,
        stack: 0,
        preserved: [0; 12],
    };
}

global_asm!(
    include_str!("switch.S"),
    resume = const offset_of!(Registers, resume),
    stack = const offset_of!(Registers, stack),
    preserved = const offset_of!(Registers, preserved),
);

unsafe extern "C" {
    fn limen_switch_context(
        outgoing: *mut Registers,
        incoming: *const Registers,
        argument: *const (),
    );
}

pub(crate) struct Context(UnsafeCell<Registers>);

impl Context {
    pub(crate) const fn new() -> Self {
        Self(UnsafeCell::new(Registers::EMPTY))
    }

    /// Requires exclusive use of an inactive context and a live stack extent.
    pub(crate) unsafe fn prepare(&self, stack: usize, entry: extern "C" fn(*const ()) -> !) {
        unsafe {
            self.0.get().write(Registers {
                resume: (entry as *const ()).addr(),
                stack,
                ..Registers::EMPTY
            });
        }
    }

    /// Requires an inactive context with no permitted future entry.
    pub(crate) unsafe fn retire(&self) {
        unsafe { self.0.get().write(Registers::EMPTY) };
    }

    // These are normal call boundaries. Integer caller-saved registers can be
    // discarded, gp and tp remain Hart-local, and FS/VS stay disabled.
    unsafe fn switch(&self, incoming: &Context, argument: *const ()) {
        unsafe { limen_switch_context(self.0.get(), incoming.0.get(), argument) };
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LeaveReceipt(Occupancy);

impl LeaveReceipt {
    pub(crate) fn occupancy(self) -> Occupancy {
        self.0
    }
}

#[derive(Clone, Copy)]
enum State {
    Idle,
    Running(Occupancy),
    Stopped(LeaveReceipt),
}

pub(crate) struct Hart {
    dispatcher: Context,
    state: Cell<State>,
    guards: Cell<usize>,
}

// Only the LocalHart for the corresponding physical Hart accesses the
// dispatcher and state. A suspended Unit never uses its old LocalHart again.
unsafe impl Sync for Hart {}

pub(crate) struct LocalHart<'a> {
    hart: &'a Hart,
    local: PhantomData<*mut ()>,
}

impl Hart {
    pub(crate) const fn new() -> Self {
        Self {
            dispatcher: Context::new(),
            state: Cell::new(State::Idle),
            guards: Cell::new(0),
        }
    }

    pub(crate) fn supported() -> bool {
        let status: usize;
        unsafe { asm!("csrr {}, sstatus", out(reg) status, options(nomem, nostack)) };
        const EXTENSIONS: usize = (3 << 13) | (3 << 9);
        boot::established() && status & EXTENSIONS == 0
    }

    /// The caller has established that this is the current physical Hart.
    pub(crate) unsafe fn local(&self) -> LocalHart<'_> {
        LocalHart {
            hart: self,
            local: PhantomData,
        }
    }
}

impl<'a> LocalHart<'a> {
    pub(crate) fn guards(&self) -> &'a Cell<usize> {
        &self.hart.guards
    }

    pub(crate) fn running(&self) -> Option<Occupancy> {
        match self.hart.state.get() {
            State::Running(occupancy) => Some(occupancy),
            _ => None,
        }
    }

    /// The Room has authorized this exact occupancy. Its live kernel stack
    /// and continuation must persist until the Room settles the receipt.
    pub(crate) unsafe fn enter(
        &self,
        occupancy: Occupancy,
        context: &Context,
        argument: *const (),
    ) {
        assert!(matches!(self.hart.state.get(), State::Idle));
        assert_eq!(self.guards().get(), 0);
        self.hart.state.set(State::Running(occupancy));
        unsafe { self.hart.dispatcher.switch(context, argument) };
        // The dispatcher is executing on its original stack again. The old
        // instruction and stack are saved but cannot execute without a new
        // Room occupancy.
        self.hart.state.set(State::Stopped(LeaveReceipt(occupancy)));
    }

    /// Requires invocation on the stack of this exact running occupancy.
    pub(crate) unsafe fn leave(&self, occupancy: Occupancy, context: &Context) {
        assert!(self.running() == Some(occupancy));
        assert_eq!(self.guards().get(), 0);
        unsafe { context.switch(&self.hart.dispatcher, core::ptr::null()) };
    }

    pub(crate) fn stopped(&self) -> Option<LeaveReceipt> {
        match self.hart.state.get() {
            State::Stopped(receipt) => Some(receipt),
            _ => None,
        }
    }

    pub(crate) fn settle(&self, receipt: LeaveReceipt) {
        assert!(self.stopped().map(LeaveReceipt::occupancy) == Some(receipt.occupancy()));
        self.hart.state.set(State::Idle);
    }
}

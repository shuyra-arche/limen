//! One firmware-admitted secondary hart and its resident supervisor stacks.

use core::{
    cell::UnsafeCell,
    mem::{MaybeUninit, offset_of},
    sync::atomic::{AtomicU32, Ordering},
};

use super::{current_hart, established_on};
use crate::metal::riscv::sbi;

const DISPATCH_BYTES: usize = 16 * 1024;
const TRAP_BYTES: usize = 4 * 1024;

#[repr(u32)]
enum State {
    Idle,
    Preparing,
    Published,
    Entered,
}

#[repr(align(16))]
struct Stack<const SIZE: usize>(UnsafeCell<[MaybeUninit<u8>; SIZE]>);

impl<const SIZE: usize> Stack<SIZE> {
    const fn new() -> Self {
        Self(UnsafeCell::new([MaybeUninit::uninit(); SIZE]))
    }

    fn top(&self) -> usize {
        self.0.get().cast::<u8>().wrapping_add(SIZE).addr()
    }
}

#[repr(C)]
struct Launch {
    hart: usize,
    stack: usize,
    trap_stack: usize,
    entry: fn(usize, usize) -> !,
    argument: usize,
}

/// Resident dispatcher and emergency stack supply for one secondary hart.
/// Use a static instance in shared Bare-addressable kernel storage.
#[repr(C)]
pub struct Secondary {
    state: AtomicU32,
    launch: UnsafeCell<MaybeUninit<Launch>>,
    stack: Stack<DISPATCH_BYTES>,
    trap_stack: Stack<TRAP_BYTES>,
}

// The state word grants exactly one writer and releases the descriptor to
// exactly one entering hart. The hardware entry is the sole stack user.
unsafe impl Sync for Secondary {}

pub(super) const PUBLISHED: u32 = State::Published as u32;
pub(super) const ENTERED: u32 = State::Entered as u32;
pub(super) const STATE_OFFSET: usize = offset_of!(Secondary, state);
pub(super) const HART_OFFSET: usize = offset_of!(Secondary, launch) + offset_of!(Launch, hart);
pub(super) const STACK_OFFSET: usize = offset_of!(Secondary, launch) + offset_of!(Launch, stack);
pub(super) const TRAP_STACK_OFFSET: usize =
    offset_of!(Secondary, launch) + offset_of!(Launch, trap_stack);

impl Secondary {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(State::Idle as u32),
            launch: UnsafeCell::new(MaybeUninit::uninit()),
            stack: Stack::new(),
            trap_stack: Stack::new(),
        }
    }

    pub(crate) fn trap_stack(&self) -> usize {
        self.trap_stack.top()
    }

    /// The caller runs in Bare supervisor mode and supplies a trusted entry
    /// within the shared kernel image. The entry must have a bounded stack
    /// footprint and accept the given argument. The stacks and launch state
    /// remain reserved after a successful firmware request.
    pub(crate) unsafe fn start(
        &'static self,
        hart: usize,
        entry: fn(usize, usize) -> !,
        argument: usize,
    ) -> Result<(), isize> {
        if hart == current_hart() {
            return Err(sbi::ALREADY_AVAILABLE);
        }
        self.state
            .compare_exchange(
                State::Idle as u32,
                State::Preparing as u32,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .map_err(|_| sbi::ALREADY_AVAILABLE)?;
        unsafe {
            self.launch.get().write(MaybeUninit::new(Launch {
                hart,
                stack: self.stack.top(),
                trap_stack: self.trap_stack(),
                entry,
                argument,
            }));
        }
        self.state.store(PUBLISHED, Ordering::Release);

        let address = super::start::enter_secondary as *const () as usize;
        let opaque = core::ptr::from_ref(self).expose_provenance();
        let result = sbi::start_hart(hart, address, opaque);
        if result.is_err() {
            // Firmware rejection leaves no hart with access to this launch.
            self.state.store(State::Idle as u32, Ordering::Release);
        }
        result
    }
}

impl Default for Secondary {
    fn default() -> Self {
        Self::new()
    }
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn limen_secondary_boot(hart: usize, opaque: usize) -> ! {
    // The assembly entry acquired the published launch before taking its stacks.
    let secondary = unsafe { &*core::ptr::with_exposed_provenance::<Secondary>(opaque) };
    let launch = unsafe { (&*secondary.launch.get()).assume_init_ref() };
    assert!(hart == current_hart() && established_on(launch.trap_stack));
    (launch.entry)(hart, launch.argument)
}

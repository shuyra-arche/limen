//! Resident launch record and private stacks for one minor CPU.

use core::{
    cell::UnsafeCell,
    mem::{MaybeUninit, offset_of},
    sync::atomic::{AtomicU32, Ordering},
};

use crate::metal::boot;
use core::ops::Range;

#[derive(Debug, Eq, PartialEq)]
pub enum StartError {
    /// The target is the caller or this launch record is already in use.
    Busy,
    /// No CPU owns this request. The slot may be retried.
    Rejected(isize),
    #[cfg(target_arch = "x86_64")]
    /// A command reached hardware. The slot and trampoline remain reserved.
    Unconfirmed(isize),
}

const DISPATCH_BYTES: usize = 16 * 1024;
#[cfg(target_arch = "riscv64")]
const TRAP_BYTES: usize = 4 * 1024;

#[repr(u32)]
enum State {
    Idle,
    Preparing,
    Published,
    Entered,
    Ready,
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
    #[cfg(target_arch = "riscv64")]
    trap_stack: usize,
    entry: fn(usize, usize) -> !,
    argument: usize,
}

/// Resident launch descriptor and private stack supply for one minor CPU.
#[repr(C)]
pub struct Minor {
    state: AtomicU32,
    launch: UnsafeCell<MaybeUninit<Launch>>,
    stack: Stack<DISPATCH_BYTES>,
    #[cfg(target_arch = "riscv64")]
    trap_stack: Stack<TRAP_BYTES>,
    pub(crate) context: boot::MinorContext,
}

// The state word grants exactly one writer and releases the descriptor to
// exactly one entering hart. The hardware entry is the sole stack user.
unsafe impl Sync for Minor {}

pub(super) const PUBLISHED: u32 = State::Published as u32;
pub(super) const ENTERED: u32 = State::Entered as u32;
pub(super) const STATE_OFFSET: usize = offset_of!(Minor, state);
pub(super) const HART_OFFSET: usize = offset_of!(Minor, launch) + offset_of!(Launch, hart);
pub(super) const STACK_OFFSET: usize = offset_of!(Minor, launch) + offset_of!(Launch, stack);
#[cfg(target_arch = "riscv64")]
pub(super) const TRAP_STACK_OFFSET: usize =
    offset_of!(Minor, launch) + offset_of!(Launch, trap_stack);

impl Minor {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(State::Idle as u32),
            launch: UnsafeCell::new(MaybeUninit::uninit()),
            stack: Stack::new(),
            #[cfg(target_arch = "riscv64")]
            trap_stack: Stack::new(),
            context: boot::minor_context(),
        }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn trap_stack(&self) -> usize {
        self.trap_stack.top()
    }

    /// Returns whether the target CPU has installed and verified its startup state.
    pub fn ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == State::Ready as u32
    }

    pub fn stack_range(&self) -> Range<usize> {
        self.stack.top() - DISPATCH_BYTES..self.stack.top()
    }

    /// Request startup of an exclusively owned, inactive CPU.
    ///
    /// # Safety
    /// Call on the major CPU after Boot delivery. The image, stacks, page
    /// tables and launch record must remain reserved and identity-addressable.
    /// The caller supplies a trusted entry within the shared kernel image.
    /// The entry must fit the minor stack and accept the supplied argument.
    /// Use the firmware-verified startup method for this target. On x86, keep
    /// the SIPI page exclusive, executable and identity-mapped, and supply a
    /// delay that waits at least the requested microseconds after firmware exit.
    pub unsafe fn start(
        &'static self,
        hart: usize,
        entry: fn(usize, usize) -> !,
        argument: usize,
        method: boot::Startup,
    ) -> Result<(), StartError> {
        if hart == boot::current_hart() {
            return Err(StartError::Busy);
        }
        self.state
            .compare_exchange(
                State::Idle as u32,
                State::Preparing as u32,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .map_err(|_| StartError::Busy)?;
        unsafe {
            self.launch.get().write(MaybeUninit::new(Launch {
                hart,
                stack: self.stack.top(),
                #[cfg(target_arch = "riscv64")]
                trap_stack: self.trap_stack(),
                entry,
                argument,
            }));
        }
        self.state.store(PUBLISHED, Ordering::Release);

        let result = unsafe { boot::wake_minor(self, hart, method) };
        if matches!(result, Err(StartError::Rejected(_))) {
            self.state.store(State::Idle as u32, Ordering::Release);
        }
        result
    }
}

impl Default for Minor {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) extern "C" fn arrive(hart: usize, minor: &Minor) -> ! {
    let launch = unsafe { (&*minor.launch.get()).assume_init_ref() };
    let sp = crate::metal::stack::pointer();
    assert!(
        hart == launch.hart && hart == boot::current_hart() && minor.stack_range().contains(&sp)
    );
    #[cfg(target_arch = "riscv64")]
    assert!(boot::established_on(launch.trap_stack));
    #[cfg(not(target_arch = "riscv64"))]
    assert!(boot::established());
    minor.state.store(State::Ready as u32, Ordering::Release);
    (launch.entry)(hart, launch.argument)
}

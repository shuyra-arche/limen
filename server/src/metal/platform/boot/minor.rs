//! Resident launch record and private stacks for one minor CPU.

use core::{
    cell::UnsafeCell,
    mem::{MaybeUninit, offset_of},
    sync::atomic::{AtomicU32, Ordering},
};

use crate::metal::{
    platform::boot,
    processor::hart::local::{self, HartLocal, stack::Stack},
};
use core::ops::Range;

#[derive(Debug, Eq, PartialEq)]
pub enum StartError {
    /// The target is the caller or this launch record is already in use.
    Busy,
    /// No CPU owns this request. The slot may be retried.
    Rejected(isize),
    /// A command reached hardware. The slot and trampoline remain reserved.
    Unconfirmed(boot::DeliveryError),
}

use super::STACK_BYTES;

#[repr(u32)]
enum State {
    Idle,
    Preparing,
    Published,
    Entered,
    Ready,
}

#[repr(C)]
struct Launch {
    hart: usize,
    stack: usize,
    entry: fn(usize, usize) -> !,
    argument: usize,
}

/// Resident launch descriptor and private stack supply for one minor CPU.
#[repr(C)]
pub struct Minor {
    state: AtomicU32,
    launch: UnsafeCell<MaybeUninit<Launch>>,
    stack: Stack<STACK_BYTES>,
    pub(crate) context: boot::MinorContext,
    local: HartLocal,
}

// The state word grants exactly one writer and releases the descriptor to
// exactly one entering hart. The hardware entry is the sole stack user.
unsafe impl Sync for Minor {}

pub(in crate::metal) const PUBLISHED: u32 = State::Published as u32;
pub(in crate::metal) const ENTERED: u32 = State::Entered as u32;
pub(in crate::metal) const STATE_OFFSET: usize = offset_of!(Minor, state);
pub(in crate::metal) const HART_OFFSET: usize =
    offset_of!(Minor, launch) + offset_of!(Launch, hart);
pub(in crate::metal) const STACK_OFFSET: usize =
    offset_of!(Minor, launch) + offset_of!(Launch, stack);
impl Minor {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(State::Idle as u32),
            launch: UnsafeCell::new(MaybeUninit::uninit()),
            stack: Stack::new(),
            context: boot::minor_context(),
            local: HartLocal::new(),
        }
    }

    /// Publish the complete launch, including all inputs the target may read.
    /// Called by the native startup owner before any wake command.
    pub(in crate::metal) fn publish(&self) {
        assert_eq!(self.state.load(Ordering::Relaxed), State::Preparing as u32);
        self.state.store(PUBLISHED, Ordering::Release);
    }

    /// Returns whether the target CPU has installed and verified its startup state.
    pub fn ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == State::Ready as u32
    }

    pub fn stack_range(&self) -> Range<usize> {
        self.stack.top() - STACK_BYTES..self.stack.top()
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
        if hart == local::current_hart().get() {
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
                entry,
                argument,
            }));
        }
        // Each native path completes its actual machine inputs before publishing.
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

pub(crate) extern "C" fn arrive(hart: usize, minor: &'static Minor) -> ! {
    let launch = unsafe { (&*minor.launch.get()).assume_init_ref() };
    let sp = local::stack_pointer();
    assert!(hart == launch.hart && minor.stack_range().contains(&sp));
    assert!(minor.established(hart));
    // `Entered` identifies this CPU as the sole initializer of this resident
    // local record. The terminal vectors cannot observe its partial setup.
    let entry = boot::entry().expect("verified minor entry");
    unsafe { minor.local.install(hart, entry) };
    assert!(local::can_switch(), "minor Hart protection is not idle");
    minor.state.store(State::Ready as u32, Ordering::Release);
    (launch.entry)(hart, launch.argument)
}

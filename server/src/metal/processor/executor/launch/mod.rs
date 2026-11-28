//! Resident launch record and private stacks for one minor CPU.

use core::{
    cell::UnsafeCell,
    mem::{MaybeUninit, offset_of},
    sync::atomic::{AtomicU32, Ordering},
};

use crate::metal::processor::{
    executor::{
        entry,
        local::{
            self, LocalStorage,
            stack::{Stack, StackExtent},
        },
    },
    treaty::can_switch,
};
use core::ops::Range;

#[derive(Debug, Eq, PartialEq)]
pub enum StartError {
    /// The target is the caller or this launch record is already in use.
    Busy,
    /// No CPU owns this request. The slot may be retried.
    Rejected(isize),
    /// A command reached hardware. The slot and trampoline remain reserved.
    Unconfirmed(native::DeliveryError),
}

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
use self::aarch64 as native;

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
use self::riscv64 as native;

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
use self::x86_64 as native;

pub use native::Startup;

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
    processor: usize,
    stack: usize,
    entry: fn(usize, usize) -> !,
    argument: usize,
}

/// Resident launch descriptor and private stack supply for one minor CPU.
#[repr(C)]
pub struct Minor {
    state: AtomicU32,
    launch: UnsafeCell<MaybeUninit<Launch>>,
    stack: StackExtent,
    context: native::MinorContext,
    local: local::Material,
}

// The state word grants exactly one writer and releases the descriptor to
// exactly one entering processor. The hardware entry is the sole stack user.
unsafe impl Sync for Minor {}

pub(in crate::metal) const PUBLISHED: u32 = State::Published as u32;
pub(in crate::metal) const ENTERED: u32 = State::Entered as u32;
pub(in crate::metal) const STATE_OFFSET: usize = offset_of!(Minor, state);
pub(in crate::metal) const PROCESSOR_OFFSET: usize =
    offset_of!(Minor, launch) + offset_of!(Launch, processor);
pub(in crate::metal) const STACK_OFFSET: usize =
    offset_of!(Minor, launch) + offset_of!(Launch, stack);
impl Minor {
    /// Construct a launch record with the original fixed startup materials.
    pub const fn new<const BYTES: usize, const N: usize>(
        stack: &'static Stack<BYTES>,
        local: &'static LocalStorage<N>,
    ) -> Self {
        Self {
            state: AtomicU32::new(State::Idle as u32),
            launch: UnsafeCell::new(MaybeUninit::uninit()),
            stack: stack.extent(),
            context: native::minor_context(),
            local: local.material(),
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
        self.stack.range()
    }

    /// Request startup of an exclusively owned, inactive CPU.
    ///
    /// # Safety
    /// Call on the major CPU after Boot delivery. The image, stacks, page
    /// tables and launch record must remain reserved and identity-addressable.
    /// Dedicate this stack and local supply to this record. Keep both reserved
    /// through an unconfirmed wake, or for the lifetime of the entered CPU.
    /// The caller supplies a trusted entry within the shared kernel image.
    /// The entry must fit the minor stack and accept the supplied argument.
    /// Use the firmware-verified startup method for this target. On x86, keep
    /// the SIPI page exclusive, executable and identity-mapped, and supply a
    /// delay that waits at least the requested microseconds after firmware exit.
    pub unsafe fn start(
        &'static self,
        processor: usize,
        entry: fn(usize, usize) -> !,
        argument: usize,
        method: &mut Startup,
    ) -> Result<(), StartError> {
        if processor
            == crate::metal::processor::treaty::try_current_processor()
                .expect("ordinary processor entry")
                .get()
        {
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
                processor,
                stack: self.stack.top(),
                entry,
                argument,
            }));
        }
        // Each native path completes its actual machine inputs before publishing.
        let result = unsafe { native::wake_minor(self, processor, method) };
        if matches!(result, Err(StartError::Rejected(_))) {
            self.state.store(State::Idle as u32, Ordering::Release);
        }
        result
    }
}

pub(crate) extern "C" fn arrive(processor: usize, minor: &'static Minor) -> ! {
    let launch = unsafe { (&*minor.launch.get()).assume_init_ref() };
    assert_eq!(processor, launch.processor);
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    assert!(entry::matches_identity(processor));
    // `Entered` identifies this CPU as the sole initializer of this resident
    // local record. The terminal vectors cannot observe its partial setup.
    let entry = entry::admit(minor.stack_range()).expect("verified minor entry");
    unsafe { minor.local.bind().install(processor, entry) };
    assert!(can_switch(), "minor processor protection is not idle");
    minor.state.store(State::Ready as u32, Ordering::Release);
    (launch.entry)(processor, launch.argument)
}

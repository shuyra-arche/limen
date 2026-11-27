//! Terminal diagnostics for early exceptions on major and minor CPUs.
use crate::metal::{platform::boot::firmware, processor::executor::local::stack::Stack};
use core::sync::atomic::AtomicPtr;

const BYTES: usize = 16 * 1024;
static mut STACK: Stack<BYTES> = Stack::new();

pub(crate) fn stack_range() -> core::ops::Range<usize> {
    let start = (&raw const STACK).addr();
    start..start + BYTES
}

// The image relocation arms the stack before any CPU can enter a vector,
// including traps during RV64 BSS initialization. Each vector atomically takes
// the pointer and leaves null. Only the winner can call Rust on this stack.
pub(in crate::metal) static TOP: AtomicPtr<u8> =
    AtomicPtr::new((&raw mut STACK).cast::<u8>().wrapping_add(BYTES));

pub(in crate::metal) struct Trap {
    pub cause: usize,
    pub pc: usize,
    pub value: usize,
    pub stack: usize,
    pub status: usize,
}

impl Trap {
    pub(in crate::metal) fn report(self) -> ! {
        let Self {
            cause,
            pc,
            value,
            stack,
            status,
        } = self;
        firmware::trap_services().reject(format_args!(
            "FAIL Limen boot trap: cause={cause:#x} pc={pc:#x} value={value:#x} stack={stack:#x} status={status:#x}\n"
        ))
    }
}

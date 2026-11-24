//! Terminal diagnostics for early exceptions on major and minor CPUs.
use crate::metal::{boot::firmware, generic::stack::Stack};
use core::sync::atomic::AtomicU32;

// A vector atomically exchanges zero for the initial one before it uses STACK.
// The stack has a single lifetime owner. A nested or simultaneous exception
// parks in the vector without touching the reporting CPU's stack. The nonzero
// initializer is essential on RV64, where the vector is armed before BSS clear.
pub(in crate::metal) static AVAILABLE: AtomicU32 = AtomicU32::new(1);
pub(in crate::metal) const BYTES: usize = 16 * 1024;
#[unsafe(link_section = ".trapstk")]
pub(in crate::metal) static mut STACK: Stack<BYTES> = Stack::new();

pub(in crate::metal) extern "C" fn report(
    cause: usize,
    pc: usize,
    value: usize,
    stack: usize,
    status: usize,
) -> ! {
    firmware::trap_services().reject(format_args!(
        "FAIL Limen boot trap: cause={cause:#x} pc={pc:#x} value={value:#x} stack={stack:#x} status={status:#x}\n"
    ))
}

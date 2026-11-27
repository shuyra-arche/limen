use crate::metal::processor::{
    description::x86_64::status,
    execution::terminal::{trap as diagnostic, x86_64::idt},
    executor::{local, x86_64::native},
};
use core::arch::asm;

pub(super) use native::boot_identity as identity;

pub(super) fn verify() -> Option<local::Entry> {
    let (flags, cr0): (usize, usize);
    unsafe {
        asm!("pushfq", "pop {}", "mov {}, cr0", out(reg) flags, out(reg) cr0);
    }
    if !status::paging(cr0) || !status::interrupts_masked(flags) {
        return None;
    }
    let descriptor = idt();
    local::Entry::admit(
        native::Entry {
            base: descriptor.base,
            limit: descriptor.limit,
        },
        diagnostic::stack_range(),
    )
}

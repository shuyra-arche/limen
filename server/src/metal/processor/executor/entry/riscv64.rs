use crate::metal::{
    interrupt::riscv64::internal::supervisor,
    memory::mmu::riscv64::internal::context::Context,
    processor::{
        execution::terminal::{riscv64::trap, trap as diagnostic},
        executor::{local, riscv64::native},
    },
};

// Both entries establish supervisor state before using either stack.
macro_rules! establish_supervisor {
    () => {
        r#"
        csrci sstatus, {interrupt_enable}
        csrw sie, zero
        li t0, {extensions}
        csrc sstatus, t0
        .option push
        .option norelax
        la gp, __global_pointer$
        .option pop
        mv tp, zero
        "#
    };
}

pub(in crate::metal) use establish_supervisor;

pub(super) fn verify() -> Option<local::Entry> {
    let status = native::status();
    if !status.masked()
        || !status.extensions_off()
        || !supervisor::all_masked()
        || Context::current() != Some(Context::BARE)
    {
        return None;
    }
    local::Entry::admit(
        native::Entry {
            vector: trap as *const () as usize,
            scratch: (&raw const diagnostic::TOP).addr(),
        },
        diagnostic::stack_range(),
    )
}

pub(crate) use super::handoff::sbi::admit;

mod major;

pub(crate) fn image() -> core::ops::Range<usize> {
    // These boundaries describe the final linked image, including the platform
    // sections supplied by its consumer. No Rust object spans that interval.
    unsafe extern "C" {
        static _kernel_base: u8;
        static _kernel_edge: u8;
    }
    (&raw const _kernel_base).addr()..(&raw const _kernel_edge).addr()
}

pub(crate) fn linked_stack() -> core::ops::Range<usize> {
    unsafe extern "C" {
        static _major_stack_start: u8;
        static _major_stack_end: u8;
    }
    (&raw const _major_stack_start).addr()..(&raw const _major_stack_end).addr()
}
